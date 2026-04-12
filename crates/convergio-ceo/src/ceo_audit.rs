//! CEO audit trail + circuit breaker for routing reliability.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::Json;
use axum::routing::get;
use axum::Router;
use convergio_db::pool::ConnPool;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};

use convergio_types::extension::Migration;

// ── Schema ───────────────────────────────────────────────────────────────────

pub fn ceo_audit_migrations() -> Vec<Migration> {
    vec![Migration {
        version: 43,
        description: "CEO routing audit log",
        up: "CREATE TABLE IF NOT EXISTS ceo_routing_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL DEFAULT (datetime('now')),
            instruction TEXT NOT NULL,
            tool_chosen TEXT,
            params_json TEXT,
            routing_method TEXT NOT NULL,
            latency_ms INTEGER NOT NULL,
            success INTEGER NOT NULL DEFAULT 1
        )",
    }]
}

// ── Audit log ────────────────────────────────────────────────────────────────

pub fn record_routing(
    pool: &ConnPool,
    instruction: &str,
    tool: Option<&str>,
    params: Option<&Value>,
    method: &str,
    latency_ms: u64,
    success: bool,
) {
    if let Ok(conn) = pool.get() {
        let params_str = params.map(|p| p.to_string());
        let _ = conn.execute(
            "INSERT INTO ceo_routing_log \
             (instruction, tool_chosen, params_json, routing_method, latency_ms, success) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                instruction,
                tool,
                params_str,
                method,
                latency_ms as i64,
                success as i32,
            ],
        );
    }
}

// ── Audit route ──────────────────────────────────────────────────────────────

pub fn ceo_audit_routes(pool: ConnPool) -> Router {
    Router::new()
        .route("/api/ceo/log", get(handle_log))
        .with_state(pool)
}

#[derive(Deserialize)]
struct LogQuery {
    limit: Option<i64>,
}

async fn handle_log(State(pool): State<ConnPool>, Query(q): Query<LogQuery>) -> Json<Value> {
    let limit = q.limit.unwrap_or(50).min(200);
    let conn = match pool.get() {
        Ok(c) => c,
        Err(e) => return Json(json!({"error": e.to_string()})),
    };
    let mut stmt = match conn.prepare(
        "SELECT id, timestamp, instruction, tool_chosen, params_json, \
         routing_method, latency_ms, success \
         FROM ceo_routing_log ORDER BY id DESC LIMIT ?1",
    ) {
        Ok(s) => s,
        Err(e) => return Json(json!({"error": e.to_string()})),
    };
    let rows: Vec<Value> = match stmt.query_map([limit], |r| {
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "timestamp": r.get::<_, String>(1)?,
            "instruction": r.get::<_, String>(2)?,
            "tool_chosen": r.get::<_, Option<String>>(3)?,
            "params_json": r.get::<_, Option<String>>(4)?,
            "routing_method": r.get::<_, String>(5)?,
            "latency_ms": r.get::<_, i64>(6)?,
            "success": r.get::<_, i32>(7)? == 1,
        }))
    }) {
        Ok(mapped) => mapped.filter_map(|r| r.ok()).collect(),
        Err(_) => vec![],
    };
    Json(json!({"entries": rows, "count": rows.len()}))
}

// ── Circuit breaker ──────────────────────────────────────────────────────────

/// In-memory circuit breaker for CEO routing.
pub struct CircuitBreaker {
    total: AtomicU64,
    errors: AtomicU64,
    open_since: std::sync::Mutex<Option<std::time::Instant>>,
}

impl CircuitBreaker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            total: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            open_since: std::sync::Mutex::new(None),
        })
    }

    /// Record a routing outcome.
    pub fn record(&self, success: bool) {
        self.total.fetch_add(1, Ordering::SeqCst);
        if !success {
            self.errors.fetch_add(1, Ordering::SeqCst);
        }
        // Reset counters every 100 calls to keep window fresh
        let total = self.total.load(Ordering::SeqCst);
        if total >= 100 {
            self.total.store(0, Ordering::SeqCst);
            self.errors.store(0, Ordering::SeqCst);
        }
        // Auto-evaluate after each recording
        self.evaluate();
    }

    /// Check if circuit is open (should use fallback).
    pub fn is_open(&self) -> bool {
        let guard = self.open_since.lock().unwrap();
        if let Some(opened) = *guard {
            // After 5 min, allow one probe
            opened.elapsed().as_secs() < 300
        } else {
            false
        }
    }

    /// Evaluate whether to open or close the circuit.
    pub fn evaluate(&self) {
        let total = self.total.load(Ordering::SeqCst);
        let errors = self.errors.load(Ordering::SeqCst);
        if total >= 20 && (errors as f64 / total as f64) > 0.05 {
            let mut guard = self.open_since.lock().unwrap();
            if guard.is_none() {
                tracing::warn!("ceo circuit breaker OPEN: {errors}/{total} errors");
                *guard = Some(std::time::Instant::now());
            }
        }
    }

    /// Close circuit after successful probe.
    pub fn close(&self) {
        let mut guard = self.open_since.lock().unwrap();
        if guard.is_some() {
            tracing::info!("ceo circuit breaker CLOSED after successful probe");
            *guard = None;
            self.total.store(0, Ordering::SeqCst);
            self.errors.store(0, Ordering::SeqCst);
        }
    }
}
