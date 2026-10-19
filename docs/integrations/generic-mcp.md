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
| `table_create` | `match` (object, or TOML/JSON text) or `file` (path) | `run_id`, `tables: [{id, name, after}]` |
| `table_status` | `table` (id or name), `full` | status, phase, `rally_log` (ball, answer, checks, note per rally), `current_ball`, `result`, `pending` |
| `table_list` | `active_only`, `run` | table summaries + count of pending balls |
| `ball_answer` | `table`, `verdict` (`accept`/`return`/`stop`), `note` | the rally answered |
| `table_stop` | `table` | stops before the next rally |
| `table_wait` | `tables` (array, optional), `timeout_secs` (60, max 600) | `reason` (`awaiting_server`/`ended`/`timeout`), `pending` balls, summaries |

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
tools/list and a full external-server table).
