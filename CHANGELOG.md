# Changelog

## v0.1.0 (2026-10-19) - First Serve

First release: tables, rallies, an external referee, parallel and serial scheduling, and a trace
of every rally.

- **Engine (`upp-core`).** Table / ball / rally / point model; leases (wall time, max rallies,
  token and cost budgets); a scheduler with a parallel cap and `after` chains (a point becomes
  the next table's `{{prev.answer}}`; a failed upstream skips the chain); an append-only SQLite
  store (WAL) that is also the bus between processes (atomic claims, stop requests, external
  server answers).
- **Players.** `shell`, `openai` (OpenAI-compatible, kept conversation, optional Halogen
  keep/fork headers), `openunum` (HTTP API, one session per table), `claude`, `codex`, `gemini`,
  `opencode` (headless CLIs, session resumed across rallies). `apply` writes an answer's code
  block to a file; `{{file:...}}` puts a file into the ball; `setup` prepares a table's cwd.
- **Referee.** `command`, `file_exists`, `http_ok`, `json_path_equals`, and `judge` (an LLM, last
  and never the table's own player). Failed checks go back with their output tail.
- **Server modes.** `auto`, `model` (a player writes the note), `external` (answered over MCP,
  HTTP, CLI or the TUI; the referee always decides whether a point is possible).
- **Front-ends.** `upp` CLI (run, create, validate, watch, list, status, pending, answer, stop,
  export, mcp) with a live TUI; `uppd` daemon (HTTP/JSON API, SSE events); MCP server with
  `table_create`, `table_status`, `table_list`, `ball_answer`, `table_stop`, `table_wait`.
- **Examples.** Fix a failing test, three parallel functions, a plan -> implement -> test chain,
  and model-free demos with scripted agents.
- **Tests.** Unit tests (config, store, process timeouts, referee, parsers for every agent CLI)
  and integration tests with scripted fake agents (points on rally 2/3, faults, parallel cap,
  chains, lease expiry, stop, model server, external server over CLI and MCP, HTTP + SSE).
