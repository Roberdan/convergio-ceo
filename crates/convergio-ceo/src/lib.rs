//! CEO routing engine — routes natural-language instructions to MCP tools via LLM.
//!
//! Flow: build tool menu → prompt LLM (Haiku) → parse JSON → validate → dispatch.

pub mod ceo_audit;
pub mod ceo_fallback;
pub mod ceo_helpers;
pub mod ceo_routes;
#[cfg(test)]
mod ceo_tests;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::ceo_helpers::*;

// ── Types ────────────────────────────────────────────────────────────────────

/// Compact tool entry for the routing prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMenuEntry {
    pub name: String,
    pub description: String,
    pub method: String,
    pub path: String,
    pub params: Vec<String>,
}

/// Routing result from the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingResult {
    pub tool: Option<String>,
    pub params: Option<Value>,
    pub error: Option<String>,
}

/// Full CEO response returned to the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CeoResponse {
    pub tool_used: Option<String>,
    pub params: Option<Value>,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub suggestion: Option<String>,
    pub routing_model: Option<String>,
    pub routing_latency_ms: Option<u64>,
}

/// CEO engine configuration.
#[derive(Debug, Clone)]
pub struct CeoEngine {
    pub daemon_url: String,
    pub api_token: Option<String>,
    client: reqwest::Client,
    tool_menu: Vec<ToolMenuEntry>,
    tool_menu_prompt: String,
}

// reqwest::Client is internally Arc-based; safe across unwind boundaries.
impl std::panic::UnwindSafe for CeoEngine {}
impl std::panic::RefUnwindSafe for CeoEngine {}

// ── Engine ───────────────────────────────────────────────────────────────────

impl CeoEngine {
    pub async fn new(daemon_url: &str, api_token: Option<&str>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        let mut engine = Self {
            daemon_url: daemon_url.to_string(),
            api_token: api_token.map(String::from),
            client,
            tool_menu: Vec::new(),
            tool_menu_prompt: String::new(),
        };
        engine.refresh_tool_menu().await;
        engine
    }

    pub async fn refresh_tool_menu(&mut self) {
        let url = format!("{}/api/meta/mcp-tools", self.daemon_url);
        let mut req = self.client.get(&url);
        if let Some(t) = &self.api_token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let tools = match req.send().await {
            Ok(resp) => match resp.json::<Value>().await {
                Ok(body) => parse_tool_defs(&body),
                Err(e) => {
                    warn!("ceo: failed to parse tool defs: {e}");
                    Vec::new()
                }
            },
            Err(e) => {
                warn!("ceo: failed to fetch tool defs: {e}");
                Vec::new()
            }
        };
        info!("ceo: loaded {} tool definitions", tools.len());
        self.tool_menu_prompt = build_tool_menu_prompt(&tools);
        self.tool_menu = tools;
    }

    /// Route a natural-language instruction to the correct tool and dispatch.
    pub async fn route_instruction(
        &self,
        instruction: &str,
        context: Option<&Value>,
    ) -> CeoResponse {
        let start = std::time::Instant::now();
        let routing_prompt = build_routing_prompt(&self.tool_menu_prompt, instruction, context);

        let (llm_response, model_used) = match self.call_inference(&routing_prompt).await {
            Ok((content, model)) => (content, Some(model)),
            Err(e) => {
                // Fallback to keyword routing when inference unavailable
                info!("ceo: inference failed ({e}), trying keyword fallback");
                if let Some((tool, params)) = crate::ceo_fallback::keyword_route(instruction) {
                    info!("ceo routing via: keyword");
                    let dispatch = self.dispatch(&tool, &params).await;
                    let latency = start.elapsed().as_millis() as u64;
                    return match dispatch {
                        Ok(result) => CeoResponse {
                            tool_used: Some(tool),
                            params: Some(params),
                            result: Some(result),
                            error: None,
                            suggestion: None,
                            routing_model: Some("keyword".into()),
                            routing_latency_ms: Some(latency),
                        },
                        Err(de) => self.error_response(de, Some("keyword".into()), &start),
                    };
                }
                return self.error_response(e, None, &start);
            }
        };

        let routing = match self.parse_with_retry(&routing_prompt, &llm_response).await {
            Ok(r) => r,
            Err(e) => return self.error_response(e, model_used.clone(), &start),
        };

        let latency = start.elapsed().as_millis() as u64;
        if routing.tool.is_none() {
            return CeoResponse {
                tool_used: None,
                params: None,
                result: None,
                error: routing.error.or(Some("No matching tool".into())),
                suggestion: find_suggestion(instruction, &self.tool_menu),
                routing_model: model_used,
                routing_latency_ms: Some(latency),
            };
        }

        let tool_name = routing.tool.unwrap();
        let params = routing.params.unwrap_or(json!({}));
        let dispatch_result = self.dispatch(&tool_name, &params).await;
        self.record_usage(&model_used, latency).await;

        match dispatch_result {
            Ok(result) => CeoResponse {
                tool_used: Some(tool_name),
                params: Some(params),
                result: Some(result),
                error: None,
                suggestion: None,
                routing_model: model_used,
                routing_latency_ms: Some(latency),
            },
            Err(e) => CeoResponse {
                tool_used: Some(tool_name),
                params: Some(params),
                result: None,
                error: Some(format!("Dispatch failed: {e}")),
                suggestion: None,
                routing_model: model_used,
                routing_latency_ms: Some(latency),
            },
        }
    }

    async fn parse_with_retry(
        &self,
        routing_prompt: &str,
        llm_response: &str,
    ) -> Result<RoutingResult, String> {
        if let Ok(r) = parse_routing_response(llm_response) {
            return Ok(r);
        }
        debug!("ceo: first parse failed, retrying with error context");
        let retry = format!(
            "{routing_prompt}\n\nPrevious response was invalid JSON: \
             {llm_response}\n\nRespond ONLY with valid JSON.",
        );
        let (content, _) = self.call_inference(&retry).await?;
        parse_routing_response(&content)
    }

    fn error_response(
        &self,
        error: String,
        model: Option<String>,
        start: &std::time::Instant,
    ) -> CeoResponse {
        CeoResponse {
            tool_used: None,
            params: None,
            result: None,
            error: Some(error),
            suggestion: None,
            routing_model: model,
            routing_latency_ms: Some(start.elapsed().as_millis() as u64),
        }
    }

    async fn call_inference(&self, prompt: &str) -> Result<(String, String), String> {
        let url = format!("{}/api/inference/complete", self.daemon_url);
        let body = json!({
            "prompt": prompt, "max_tokens": 256,
            "tier_hint": "t1", "agent_id": "ceo-router",
        });
        let mut req = self.client.post(&url).json(&body);
        if let Some(t) = &self.api_token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let resp = req
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| format!("inference HTTP error: {e}"))?;
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        let content = json
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let model = json
            .get("model_used")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        Ok((content, model))
    }

    async fn dispatch(&self, tool_name: &str, params: &Value) -> Result<Value, String> {
        let entry = self
            .tool_menu
            .iter()
            .find(|t| t.name == tool_name)
            .ok_or_else(|| format!("Unknown tool: {tool_name}"))?;

        let mut path = entry.path.clone();
        for p in &entry.params {
            if let Some(val) = params.get(p.as_str()) {
                let raw = match val {
                    Value::Number(n) => n.to_string(),
                    Value::String(s) => s.clone(),
                    _ => val.to_string(),
                };
                // URL-encode to prevent path injection
                let encoded = urlencoding::encode(&raw);
                path = path.replace(&format!(":{p}"), &encoded);
            }
        }
        let url = format!("{}{path}", self.daemon_url);
        let resp = match entry.method.as_str() {
            "POST" => {
                let body = strip_path_params(params, &entry.params);
                let mut r = self.client.post(&url).json(&body);
                if let Some(t) = &self.api_token {
                    r = r.header("Authorization", format!("Bearer {t}"));
                }
                r.send().await.map_err(|e| e.to_string())?
            }
            "PUT" => {
                let body = strip_path_params(params, &entry.params);
                let mut r = self.client.put(&url).json(&body);
                if let Some(t) = &self.api_token {
                    r = r.header("Authorization", format!("Bearer {t}"));
                }
                r.send().await.map_err(|e| e.to_string())?
            }
            "DELETE" => {
                let mut r = self.client.delete(&url);
                if let Some(t) = &self.api_token {
                    r = r.header("Authorization", format!("Bearer {t}"));
                }
                r.send().await.map_err(|e| e.to_string())?
            }
            _ => {
                let mut r = self.client.get(&url);
                if let Some(t) = &self.api_token {
                    r = r.header("Authorization", format!("Bearer {t}"));
                }
                r.send().await.map_err(|e| e.to_string())?
            }
        };
        resp.error_for_status()
            .map_err(|e| format!("dispatch HTTP error: {e}"))?
            .json::<Value>()
            .await
            .map_err(|e| e.to_string())
    }

    async fn record_usage(&self, model: &Option<String>, latency_ms: u64) {
        let url = format!("{}/api/tracking/tokens", self.daemon_url);
        let body = json!({
            "agent": "ceo-router",
            "model": model.as_deref().unwrap_or("unknown"),
            "input_tokens": 256, "output_tokens": 64, "cost_usd": 0.0001,
            "execution_host": format!("ceo-routing-{}ms", latency_ms),
        });
        let mut req = self.client.post(&url).json(&body);
        if let Some(t) = &self.api_token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        if let Err(e) = req.send().await {
            debug!("ceo: failed to record usage: {e}");
        }
    }
}
