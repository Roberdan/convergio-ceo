//! CEO routes — POST /api/ceo endpoint for LLM-routed tool dispatch.

use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use axum::routing::post;
use axum::Router;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::CeoEngine;

/// Shared CEO engine state, initialized lazily on first request.
pub struct CeoState {
    engine: RwLock<Option<CeoEngine>>,
    daemon_url: String,
    api_token: Option<String>,
}

impl CeoState {
    pub fn new(daemon_url: &str, api_token: Option<&str>) -> Self {
        Self {
            engine: RwLock::new(None),
            daemon_url: daemon_url.to_string(),
            api_token: api_token.map(String::from),
        }
    }

    async fn get_engine(&self) -> CeoEngine {
        // Fast path: engine already initialized
        {
            let guard = self.engine.read().await;
            if let Some(ref engine) = *guard {
                return engine.clone();
            }
        }
        // Slow path: initialize
        let engine = CeoEngine::new(&self.daemon_url, self.api_token.as_deref()).await;
        let cloned = engine.clone();
        {
            let mut guard = self.engine.write().await;
            *guard = Some(engine);
        }
        cloned
    }
}

#[derive(Debug, Deserialize)]
pub struct CeoRequest {
    pub instruction: String,
    pub context: Option<Value>,
}

pub fn ceo_routes(daemon_url: &str, api_token: Option<&str>) -> Router {
    let state = Arc::new(CeoState::new(daemon_url, api_token));
    Router::new()
        .route("/api/ceo", post(handle_ceo))
        .with_state(state)
}

#[tracing::instrument(skip_all, fields(instruction))]
async fn handle_ceo(
    State(state): State<Arc<CeoState>>,
    Json(req): Json<CeoRequest>,
) -> Json<Value> {
    tracing::Span::current().record("instruction", req.instruction.as_str());
    tracing::info!("ceo: routing instruction");

    let engine = state.get_engine().await;
    let response = engine
        .route_instruction(&req.instruction, req.context.as_ref())
        .await;

    Json(json!({
        "tool_used": response.tool_used,
        "params": response.params,
        "result": response.result,
        "error": response.error,
        "suggestion": response.suggestion,
        "routing_model": response.routing_model,
        "routing_latency_ms": response.routing_latency_ms,
    }))
}
