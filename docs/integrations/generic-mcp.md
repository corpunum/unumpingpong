# Any MCP client

`upp mcp` is a standard MCP server over stdio (protocol `2025-06-18`, newline-delimited JSON-RPC).

| Field | Value |
| --- | --- |
| command | `upp` (or the absolute path) |
| args | `["mcp"]`; `["mcp", "--queue-only"]` when `uppd` runs the tables; `["--db", "/path/upp.db", "mcp"]` to pick a store |
| transport | stdio |
| env | none required; `UPP_DB` selects the store |

```json
{ "mcpServers": { "unumpingpong": { "command": "upp", "args": ["mcp"] } } }
```

## Tools

| Tool | Arguments | Result |
| --- | --- | --- |
| `table_create` | `match` (object, or TOML/JSON text) or `file` (path); `parent` (a running table id: open sub-tables) | `run_id`, `tables: [{id, name, after}]` |
| `table_status` | `table` (id or name), `full` | status, `rally_log` (one **card** per rally; ball/answer/checks only with `full`), `token_split`, `children`, `worktree`, `final_card`, `pending` |
| `table_list` | `active_only`, `run` | table summaries (player and orchestrator tokens) + count of pending balls |
| `cards_wait` | `tables`, `run`, `since` (cursor), `timeout_secs` (60, max 600) | `reason` (`needs_answer`/`cards`/`ended`/`timeout`), `cursor`, `cards` |
| `card_answer` | `table`, `verdict` (`accept`/`return`/`stop`), `note`, `next` (chain a ball), `open` (new table object) | the rally answered, `created` tables |
| `transcript` | `table`, `rally` (optional) | full balls, answers, referee output, provenance (the expensive read) |
| `ball_answer` | `table`, `verdict`, `note` | the rally answered (v0.1 name; `card_answer` without `next`/`open`) |
| `table_stop` | `table` | cancels the running player call (process group killed), stops its sub-tables |
| `table_wait` | `tables` (array, optional), `timeout_secs` (60, max 600) | `reason` (`awaiting_server`/`ended`/`timeout`), `pending` balls, summaries |
| `integrate` | `run`, `into`, `check` | one branch merging every verified worktree point of the run |

A card (what an orchestrator reads instead of a transcript):

```json
{"card_id": 7, "table_id": "t3f2a9c1b0e", "table": "fix-parser", "rally": 2, "kind": "rally",
 "needs_answer": true, "done": true, "summary": "week 53 handled; 14 tests pass",
 "evidence": ["cargo test -p parser -> 14 passed"], "open_issues": [],
 "artifacts": ["src/date.rs"], "source": "block", "tokens": 3120, "elapsed_s": 41.2,
 "referee": {"passed": true, "failed": [], "tail": null},
 "provenance": {"commit": "9f8e7d6", "tree": "4b825dc", "dirty": true}, "children": []}
```

`kind = "final"` cards add `status`, `reason`, `rallies` and the table's token split.

A single-table `match`:

```json
{
  "name": "fix-tests",
  "goal": "cargo test passes",
  "cwd": "/abs/path/to/repo",
  "player": {"kind": "codex", "args": ["--dangerously-bypass-approvals-and-sandbox"]},
  "referee": [{"kind": "command", "run": "cargo test", "timeout_secs": 600}],
  "lease": {"wall_secs": 1200, "max_rallies": 4},
  "server": {"mode": "external"}
}
```

Use absolute `cwd` paths over MCP and HTTP: relative ones resolve against the process that runs
the table.

## Hand-rolled check

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"check","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | upp --db /tmp/check.db mcp --queue-only
```

**Status: protocol tested** (`crates/upp/tests/cli.rs` drives `upp mcp` through initialize,
tools/list, a full external-server table, and a two-table cards_wait / card_answer / transcript
orchestration loop).
