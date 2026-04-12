//! CEO helpers — prompt building, JSON parsing, tool menu, suggestions.

use serde_json::{json, Value};

use crate::{RoutingResult, ToolMenuEntry};

pub fn parse_tool_defs(body: &Value) -> Vec<ToolMenuEntry> {
    body.get("tools")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let name = v.get("name")?.as_str()?;
                    let desc = v.get("description")?.as_str()?;
                    let method = v.get("method")?.as_str().unwrap_or("POST");
                    let path = v.get("path")?.as_str()?;
                    let params: Vec<String> = v
                        .get("path_params")
                        .and_then(|p| p.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(ToolMenuEntry {
                        name: name.to_string(),
                        description: desc.to_string(),
                        method: method.to_string(),
                        path: path.to_string(),
                        params,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn build_tool_menu_prompt(tools: &[ToolMenuEntry]) -> String {
    let mut menu = String::from("Available tools:\n");
    for t in tools {
        let params_str = if t.params.is_empty() {
            String::new()
        } else {
            format!(" — params: {}", t.params.join(", "))
        };
        menu.push_str(&format!("- {}: {}{}\n", t.name, t.description, params_str));
    }
    menu
}

pub fn build_routing_prompt(tool_menu: &str, instruction: &str, context: Option<&Value>) -> String {
    let ctx_str = context
        .map(|c| format!("\n\nContext: {c}"))
        .unwrap_or_default();

    format!(
        "You are a tool router. Given a user instruction, select the best \
         matching tool and extract its parameters.\n\
         Respond ONLY with JSON: {{\"tool\": \"name\", \"params\": {{}}}}\n\
         If no tool matches: {{\"tool\": null, \"error\": \"reason\"}}\n\n\
         {tool_menu}\n\
         Instruction: {instruction}{ctx_str}"
    )
}

pub fn parse_routing_response(content: &str) -> Result<RoutingResult, String> {
    let trimmed = content.trim();
    let json_str = if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            &trimmed[start..=end]
        } else {
            trimmed
        }
    } else {
        trimmed
    };
    serde_json::from_str::<RoutingResult>(json_str).map_err(|e| format!("JSON parse error: {e}"))
}

pub fn find_suggestion(instruction: &str, tools: &[ToolMenuEntry]) -> Option<String> {
    let lower = instruction.to_lowercase();
    let matches: Vec<&str> = tools
        .iter()
        .filter(|t| {
            let name_lower = t.name.to_lowercase();
            let desc_lower = t.description.to_lowercase();
            lower
                .split_whitespace()
                .any(|word| name_lower.contains(word) || desc_lower.contains(word))
        })
        .map(|t| t.name.as_str())
        .take(3)
        .collect();

    if matches.is_empty() {
        None
    } else {
        Some(format!("Did you mean: {}?", matches.join(", ")))
    }
}

pub fn strip_path_params(args: &Value, path_params: &[String]) -> Value {
    match args.as_object() {
        Some(obj) => {
            let filtered: serde_json::Map<String, Value> = obj
                .iter()
                .filter(|(k, _)| !path_params.iter().any(|p| p == k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Value::Object(filtered)
        }
        None => json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_routing_valid() {
        let input = r#"{"tool": "cvg_health", "params": {}}"#;
        let result = parse_routing_response(input).unwrap();
        assert_eq!(result.tool.unwrap(), "cvg_health");
    }

    #[test]
    fn parse_routing_null_tool() {
        let input = r#"{"tool": null, "error": "No match"}"#;
        let result = parse_routing_response(input).unwrap();
        assert!(result.tool.is_none());
        assert_eq!(result.error.unwrap(), "No match");
    }

    #[test]
    fn parse_routing_with_markdown_wrapper() {
        let input = "```json\n{\"tool\": \"cvg_list_plans\", \"params\": {}}\n```";
        let result = parse_routing_response(input).unwrap();
        assert_eq!(result.tool.unwrap(), "cvg_list_plans");
    }

    #[test]
    fn build_menu_includes_all_tools() {
        let tools = vec![
            ToolMenuEntry {
                name: "cvg_health".into(),
                description: "Health check".into(),
                method: "GET".into(),
                path: "/api/health".into(),
                params: vec![],
            },
            ToolMenuEntry {
                name: "cvg_start_plan".into(),
                description: "Start a plan".into(),
                method: "POST".into(),
                path: "/api/plan-db/start/:plan_id".into(),
                params: vec!["plan_id".into()],
            },
        ];
        let menu = build_tool_menu_prompt(&tools);
        assert!(menu.contains("cvg_health"));
        assert!(menu.contains("cvg_start_plan"));
        assert!(menu.contains("plan_id"));
    }

    #[test]
    fn suggestion_finds_partial_match() {
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
        let suggestion = find_suggestion("show plans", &tools);
        assert!(suggestion.is_some());
        assert!(suggestion.unwrap().contains("cvg_list_plans"));
    }

    #[test]
    fn strip_path_params_works() {
        let args = json!({"plan_id": 42, "status": "active"});
        let result = strip_path_params(&args, &["plan_id".into()]);
        assert!(result.get("plan_id").is_none());
        assert_eq!(result.get("status").unwrap(), "active");
    }
}
