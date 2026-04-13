//! CEO dispatch — HTTP dispatch to daemon tools + usage recording.

use serde_json::{json, Value};
use tracing::debug;

use crate::ceo_helpers::strip_path_params;
use crate::CeoEngine;

impl CeoEngine {
    pub(crate) async fn dispatch(&self, tool_name: &str, params: &Value) -> Result<Value, String> {
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

    pub(crate) async fn record_usage(&self, model: &Option<String>, latency_ms: u64) {
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
