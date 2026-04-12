# ADR-032: CEO Pattern — Single MCP Tool with LLM Routing

## Status: Accepted

## Context

The MCP server exposes 129 tools to LLM clients, consuming ~15-20K tokens of context
per session. The compact profile (#685) reduced this to ~35, but the fundamental
problem remains: the client must understand the full tool surface to use it.

Meanwhile, every client (Claude Code, Copilot, Queen, web UI) implements its own
tool-calling logic. There's no centralized intelligence layer.

## Decision

Replace all external MCP tools with a **single `cvg_ceo` tool** that accepts natural
language instructions. The daemon routes internally using a cheap LLM (Haiku/MLX).

### Architecture

```
Client → cvg_ceo("start plan 1402")
              ↓
         POST /api/ceo
              ↓
         1. Load tool registry (cached, ~130 tools with descriptions)
         2. Build routing prompt (instruction + compact tool menu)
         3. Call inference T1/Haiku: "pick tool + extract params"
         4. Validate params against tool JSON schema
         5. Call internal endpoint (same as MCP dispatch)
         6. Return result to client
```

### System Prompt for Router

```
You are a tool router for the Convergio platform.
Given a user instruction, select the most appropriate tool and extract parameters.

Available tools (name: description — required params):
{TOOL_MENU}

Respond with ONLY valid JSON:
{"tool": "tool_name", "params": {"key": "value"}}

If the instruction is ambiguous, pick the most likely tool.
If no tool matches, respond: {"tool": null, "error": "No matching tool for: <instruction>"}
```

The TOOL_MENU is built from the MCP registry at startup — compact format:
```
cvg_start_plan: Start plan execution — plan_id (int)
cvg_list_plans: List plans — status? (string)
cvg_spawn_agent: Spawn agent — agent_name (str), org_id (str), instructions (str)
...
```

~50 tokens per tool × 35 tools (compact profile) = ~1750 tokens per routing call.
At Haiku pricing ($0.80/M input): ~$0.0014 per routing call. Negligible.

### Model Routing (Rule 29)

| Role | Model | Tier | Cost/M input |
|------|-------|------|-------------|
| CEO routing | Haiku | T1 | $0.80 |
| Thor validation | Haiku | T1 | $0.80 |
| Execution (effort 1-2) | Sonnet | T2 | $3.00 |
| Planning, review, architecture | Opus | T4 | $15.00 |

### Validation

After Haiku picks a tool, validate params against the tool's JSON schema:
1. Check all required params are present
2. Check types match (string, int, bool)
3. If validation fails: retry once with error message in prompt
4. If retry fails: return error to client with suggested correction

### Fallback

If inference is unavailable (daemon offline, no API key, rate limited):
1. Try MLX local model (zero cost)
2. If MLX unavailable: fall back to keyword matching (regex patterns)
3. If keyword fails: return full tool list and let client pick

### MCP Integration

The MCP server exposes ONE tool in compact+ profile:

```json
{
  "name": "cvg_ceo",
  "description": "Send a natural language instruction to Convergio. Routes to the correct internal tool automatically.",
  "parameters": {
    "instruction": {"type": "string", "description": "What you want Convergio to do"},
    "context": {"type": "object", "description": "Optional structured context (plan_id, org_id, etc.)"}
  }
}
```

Existing profiles remain:
- `full`: all 129 tools (backward compat)
- `compact`: 35 essential tools (current)
- `ceo`: just `cvg_ceo` (new, default after validation)

### What This Enables

1. **Any client works**: web UI, CLI, Telegram bot — all send natural language
2. **Centralized optimization**: improve routing once, all clients benefit
3. **Model-agnostic**: client doesn't need to understand tools
4. **Measurable**: every routing call tracked in token_usage
5. **Upgradable**: swap Haiku for fine-tuned local model later

### Hybrid approach (from Devil's Advocate review)

The DA identified real risks: latency overhead, silent misrouting, O(n) menu cost.
Mitigations adopted:

1. **Hybrid tool exposure**: keep 8-10 high-frequency tools as direct MCP alongside
   `cvg_ceo`. Route long-tail through CEO. High-frequency: health, list_plans,
   start_plan, spawn_agent, update_task, record_evidence, knowledge_search, validate_plan.
2. **Audit trail**: log EVERY routing decision (instruction, tool chosen, params, confidence)
   to a `ceo_routing_log` table. Enables post-hoc analysis of misroute rate.
3. **Prompt caching**: use Anthropic's prompt caching for the tool menu system prompt.
   Menu stays in cache across calls — amortizes the 1750-token cost.
4. **A/B rollout**: `ceo` profile runs alongside `compact`. Measure misroute rate on
   real traffic before making `ceo` the default. Rollback = switch profile env var.
5. **Thor stays Sonnet, not Haiku**: last gate before ship needs reliable reasoning.
6. **Circuit breaker**: if Haiku error rate > 5% in a 10-min window, auto-fallback
   to compact profile (direct MCP) and alert via observatory.

## Consequences

- Adds ~150ms latency per CEO-routed tool call (cached prompt reduces this)
- Direct high-frequency tools add 0ms latency (no routing)
- Misroute risk mitigated by audit trail + A/B + circuit breaker
- Requires inference API key or local model to be configured
- Full/compact profiles remain as escape hatch — no point of no return
