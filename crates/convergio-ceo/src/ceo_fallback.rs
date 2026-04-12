//! CEO keyword fallback — regex-based routing when inference is unavailable.

use serde_json::{json, Value};
use std::sync::OnceLock;

/// A keyword routing pattern: regex → (tool_name, param_extractor).
struct KeywordPattern {
    regex: regex_lite::Regex,
    tool: &'static str,
    extract: fn(&regex_lite::Captures) -> Value,
}

fn no_params(_: &regex_lite::Captures) -> Value {
    json!({})
}

fn plan_id_param(caps: &regex_lite::Captures) -> Value {
    let id = caps.get(1).map(|m| m.as_str()).unwrap_or("0");
    json!({"plan_id": id.parse::<i64>().unwrap_or(0)})
}

fn name_param(caps: &regex_lite::Captures) -> Value {
    let name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    json!({"agent_name": name})
}

fn query_param(caps: &regex_lite::Captures) -> Value {
    let q = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    json!({"query": q})
}

fn patterns() -> &'static Vec<KeywordPattern> {
    static PATTERNS: OnceLock<Vec<KeywordPattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            kp(r"(?i)^health\b", "cvg_health", no_params),
            kp(r"(?i)^doctor\b", "cvg_doctor_run", no_params),
            kp(r"(?i)^list\s+plans?\b", "cvg_list_plans", no_params),
            kp(
                r"(?i)^start\s+plan\s+(\d+)",
                "cvg_start_plan",
                plan_id_param,
            ),
            kp(
                r"(?i)^cancel\s+plan\s+(\d+)",
                "cvg_cancel_plan",
                plan_id_param,
            ),
            kp(
                r"(?i)^resume\s+plan\s+(\d+)",
                "cvg_resume_plan",
                plan_id_param,
            ),
            kp(r"(?i)^get\s+plan\s+(\d+)", "cvg_get_plan", plan_id_param),
            kp(
                r"(?i)^validate\s+plan\s+(\d+)",
                "cvg_validate_plan",
                plan_id_param,
            ),
            kp(r"(?i)^spawn\s+(\S+)", "cvg_spawn_agent", name_param),
            kp(
                r"(?i)^list\s+agents?\b",
                "cvg_list_agent_catalog",
                no_params,
            ),
            kp(r"(?i)^list\s+orgs?\b", "cvg_list_orgs", no_params),
            kp(r"(?i)^search\s+(.+)", "cvg_knowledge_search", query_param),
            kp(r"(?i)^who\b", "cvg_who", no_params),
            kp(r"(?i)^status\b", "cvg_health", no_params),
            kp(r"(?i)^cost\b", "cvg_cost_summary", no_params),
        ]
    })
}

fn kp(
    pattern: &str,
    tool: &'static str,
    extract: fn(&regex_lite::Captures) -> Value,
) -> KeywordPattern {
    KeywordPattern {
        regex: regex_lite::Regex::new(pattern).expect("invalid keyword pattern"),
        tool,
        extract,
    }
}

/// Try to route an instruction via keyword matching (no LLM needed).
/// Returns Some((tool_name, params)) on match, None otherwise.
pub fn keyword_route(instruction: &str) -> Option<(String, Value)> {
    let trimmed = instruction.trim();
    for pattern in patterns().iter() {
        if let Some(caps) = pattern.regex.captures(trimmed) {
            return Some((pattern.tool.to_string(), (pattern.extract)(&caps)));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_health() {
        let (tool, _) = keyword_route("health check").unwrap();
        assert_eq!(tool, "cvg_health");
    }

    #[test]
    fn routes_start_plan() {
        let (tool, params) = keyword_route("start plan 1402").unwrap();
        assert_eq!(tool, "cvg_start_plan");
        assert_eq!(params["plan_id"], 1402);
    }

    #[test]
    fn routes_list_plans() {
        let (tool, _) = keyword_route("list plans").unwrap();
        assert_eq!(tool, "cvg_list_plans");
    }

    #[test]
    fn routes_spawn() {
        let (tool, params) = keyword_route("spawn baccio").unwrap();
        assert_eq!(tool, "cvg_spawn_agent");
        assert_eq!(params["agent_name"], "baccio");
    }

    #[test]
    fn routes_search() {
        let (tool, params) = keyword_route("search auth middleware").unwrap();
        assert_eq!(tool, "cvg_knowledge_search");
        assert_eq!(params["query"], "auth middleware");
    }

    #[test]
    fn no_match_returns_none() {
        assert!(keyword_route("do something random and weird").is_none());
    }

    #[test]
    fn case_insensitive() {
        let (tool, _) = keyword_route("HEALTH").unwrap();
        assert_eq!(tool, "cvg_health");
    }
}
