# Changelog

## v0.1.1 (2026-10-19) - Fail closed

Correctness fixes from an external review: nothing may score a point without evidence.

- **Referee: an empty check list fails.** v0.1 passed a table with no checks. A table now needs
  at least one check (`upp validate` and the API reject it otherwise); the explicit opt-in
  `unverified = true` (per table or in `[defaults]`) allows a table without checks, and its point
  is recorded as `UNVERIFIED point on rally N` with an `unverified` check row.
- **Model server reviews fail closed.** With `review_points = true`, a reviewer error used to
  grant the point because the referee had passed. The review is now retried (3 attempts), then
  the table ends as `error`, never `point`. The reviewer's verdict must be the word `ACCEPT`
  (`ACCEPTABLE?` is not); judges likewise need the word `PASS` (`PASSED?` is not).
- **OpenUnum player: only its own turn's reply.** Each rally sends its own `turnId`, waits out a
  turn still running in the session (discarding its result), refuses a `202` that queued the
  message into another turn, polls `/api/chat/pending` until a completed payload with *its*
  `turnId` arrives, and treats `ok: false`, `cancelled`, `timedOut`/`timeout` and empty replies
  as faults.
- **Agent CLI players: exit 0 required.** A nonzero exit is a fault for claude, codex, gemini
  and opencode even when the output parses. Codex `turn.failed`/`error` and opencode `error`
  events are faults even when text was produced first.
- **Negative tests** for each: a no-op "done" answer, a table without checks, a reviewer outage,
  a reviewer that does not say `ACCEPT`, a nonzero exit with correct work on disk, a codex
  `turn.failed` after an agent message, an OpenUnum `202` queued into a running turn, a busy
  session, another turn's late result, and cancelled / timed-out / failed turns.

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
