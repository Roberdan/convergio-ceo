//! CEO pattern unit and integration tests.

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use serde_json::json;

    use crate::ceo_audit::CircuitBreaker;
    use crate::ceo_fallback::keyword_route;
    use crate::ceo_helpers::*;
    use crate::ToolMenuEntry;

    // ── Keyword fallback tests ───────────────────────────────────────────────

    #[test]
    fn keyword_routes_health() {
        let (tool, _) = keyword_route("health check").unwrap();
        assert_eq!(tool, "cvg_health");
    }

    #[test]
    fn keyword_routes_list_plans() {
        let (tool, _) = keyword_route("list plans").unwrap();
        assert_eq!(tool, "cvg_list_plans");
    }

    #[test]
    fn keyword_routes_start_plan_with_id() {
        let (tool, params) = keyword_route("start plan 1402").unwrap();
        assert_eq!(tool, "cvg_start_plan");
        assert_eq!(params["plan_id"], 1402);
    }

    #[test]
    fn keyword_ambiguous_instruction_returns_none() {
        assert!(keyword_route("do something random").is_none());
    }

    #[test]
    fn keyword_routes_spawn_with_name() {
        let (tool, params) = keyword_route("spawn baccio").unwrap();
        assert_eq!(tool, "cvg_spawn_agent");
        assert_eq!(params["agent_name"], "baccio");
    }

    #[test]
    fn keyword_routes_search_with_query() {
        let (tool, params) = keyword_route("search auth middleware").unwrap();
        assert_eq!(tool, "cvg_knowledge_search");
        assert_eq!(params["query"], "auth middleware");
    }

    #[test]
    fn keyword_case_insensitive() {
        let (tool, _) = keyword_route("HEALTH").unwrap();
        assert_eq!(tool, "cvg_health");
        let (tool, _) = keyword_route("List Plans").unwrap();
        assert_eq!(tool, "cvg_list_plans");
    }

    // ── Routing prompt + parsing tests ───────────────────────────────────────

    #[test]
    fn parse_routing_valid_json() {
        let input = r#"{"tool": "cvg_health", "params": {}}"#;
        let result = parse_routing_response(input).unwrap();
        assert_eq!(result.tool.unwrap(), "cvg_health");
    }

    #[test]
    fn parse_routing_null_tool_with_error() {
        let input = r#"{"tool": null, "error": "No match"}"#;
        let result = parse_routing_response(input).unwrap();
        assert!(result.tool.is_none());
        assert_eq!(result.error.unwrap(), "No match");
    }

    #[test]
    fn parse_routing_markdown_wrapped() {
        let input = "```json\n{\"tool\": \"cvg_list_plans\", \"params\": {}}\n```";
        let result = parse_routing_response(input).unwrap();
        assert_eq!(result.tool.unwrap(), "cvg_list_plans");
    }

    #[test]
    fn parse_routing_invalid_json_returns_error() {
        let result = parse_routing_response("not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn parse_routing_with_params() {
        let input = r#"{"tool": "cvg_start_plan", "params": {"plan_id": 1402}}"#;
        let result = parse_routing_response(input).unwrap();
        assert_eq!(result.tool.unwrap(), "cvg_start_plan");
        assert_eq!(result.params.unwrap()["plan_id"], 1402);
    }

    // ── Suggestion tests ─────────────────────────────────────────────────────

    #[test]
    fn suggestion_finds_plan_related() {
        let tools = vec![
            ToolMenuEntry {
                name: "cvg_list_plans".into(),
                description: "List execution plans".into(),
                method: "GET".into(),
                path: "/api/plan-db/list".into(),
                params: vec![],
            },
            ToolMenuEntry {
                name: "cvg_health".into(),
                description: "Health check".into(),
                method: "GET".into(),
                path: "/api/health".into(),
                params: vec![],
            },
        ];
        let suggestion = find_suggestion("show me plans", &tools);
        assert!(suggestion.is_some());
        assert!(suggestion.unwrap().contains("cvg_list_plans"));
    }

    #[test]
    fn suggestion_returns_none_for_no_match() {
        let tools = vec![ToolMenuEntry {
            name: "cvg_health".into(),
            description: "Health check".into(),
            method: "GET".into(),
            path: "/api/health".into(),
            params: vec![],
        }];
        let suggestion = find_suggestion("zzz_nonexistent_zzz", &tools);
        assert!(suggestion.is_none());
    }

    // ── Circuit breaker tests ────────────────────────────────────────────────

    #[test]
    fn circuit_starts_closed() {
        let cb = CircuitBreaker::new();
        assert!(!cb.is_open());
    }

    #[test]
    fn circuit_opens_on_high_error_rate() {
        let cb = CircuitBreaker::new();
        // 20 calls, 2 errors = 10% > 5% threshold
        for _ in 0..18 {
            cb.record(true);
        }
        for _ in 0..2 {
            cb.record(false);
        }
        cb.evaluate();
        assert!(cb.is_open());
    }

    #[test]
    fn circuit_stays_closed_on_low_error_rate() {
        let cb = CircuitBreaker::new();
        for _ in 0..20 {
            cb.record(true);
        }
        cb.evaluate();
        assert!(!cb.is_open());
    }

    #[test]
    fn circuit_closes_after_successful_probe() {
        let cb = CircuitBreaker::new();
        for _ in 0..18 {
            cb.record(true);
        }
        for _ in 0..2 {
            cb.record(false);
        }
        cb.evaluate();
        assert!(cb.is_open());
        cb.close();
        assert!(!cb.is_open());
    }

    // ── Tool menu tests ──────────────────────────────────────────────────────

    #[test]
    fn build_menu_includes_params() {
        let tools = vec![ToolMenuEntry {
            name: "cvg_start_plan".into(),
            description: "Start a plan".into(),
            method: "POST".into(),
            path: "/api/plan-db/start/:plan_id".into(),
            params: vec!["plan_id".into()],
        }];
        let menu = build_tool_menu_prompt(&tools);
        assert!(menu.contains("plan_id"));
        assert!(menu.contains("cvg_start_plan"));
    }

    #[test]
    fn strip_path_params_removes_correctly() {
        let args = json!({"plan_id": 42, "status": "active"});
        let result = strip_path_params(&args, &["plan_id".into()]);
        assert!(result.get("plan_id").is_none());
        assert_eq!(result.get("status").unwrap(), "active");
    }
}
