---
name: unumpingpong
description: Run orchestrator <-> worker "ping-pong" tables with the `upp` CLI (or the unumpingpong MCP tools). Use it to hand a well-defined coding task to a worker agent or model (Claude Code, Codex, Gemini CLI, opencode, OpenUnum, any OpenAI-compatible endpoint, any command) with an external referee (tests, commands, files, HTTP, JSON) that decides when it is done; to run several such tasks in parallel or chain them (plan -> implement -> test); and to review workers' answers as the server.
---

# UnumPingPong

A **table** gives a **ball** (a brief) to a **player** (worker). After every answer an external
**referee** runs deterministic checks. Fail: the ball goes back to the same player session with
the referee output and a server note. Pass: the table scores a **point**. Tables run in
parallel (cap) and chain in series (`after`). Everything is recorded in a SQLite store.

## When to use

- A task has a mechanical definition of done (a test command, a file, an endpoint, a JSON value)
  and a worker can do it without your whole context.
- Several independent tasks: open one table each and let them run in parallel.
- A pipeline where each step's verified output feeds the next (`after = "previous"`).
- You want evidence, not a claim: the worker never grades itself.

Not for: tasks without a checkable outcome (use a `judge` only as a last resort), or tiny edits
faster to do yourself.

## Write a match file

```toml
[defaults]
max_parallel = 3
lease = { wall_secs = 900, max_rallies = 4 }        # optional: max_tokens, max_cost_usd
player = { kind = "openai", base_url = "http://127.0.0.1:8080/v1", model = "local-model" }

[[table]]
name = "fix-parser"
goal = "cargo test -p parser passes without changing tests"
cwd = "/abs/path/to/worktree"                       # isolate workers: one worktree per table
ball = """
The parser tests fail on ISO week dates. Fix src/date.rs.
```rust
{{file:src/date.rs}}
```
"""
apply = { write_file = "src/date.rs" }              # chat models: answer = new file content
referee = [{ kind = "command", run = "cargo test -p parser", timeout_secs = 600 }]
```

Players: `shell` (`command`), `openai` (`base_url`, `model`, `api_key_env`, `halogen_keep`),
`openunum` (`base_url`), `claude` / `codex` / `gemini` / `opencode` (`model`, `args` for their
own permission flags). Agent CLIs edit files themselves: drop `apply` for them.

Referee checks: `command` (`run`, `expect_regex`, `timeout_secs`), `file_exists` (`path`,
`contains`), `http_ok` (`url`, `status`), `json_path_equals` (`path`, `equals`, `file`/`url`/the
answer), `judge` (`player` different from the table's, runs last).

Every table needs at least one referee check; a table without one is rejected unless it sets
`unverified = true`, and then its point is marked UNVERIFIED. Prefer a real check.

Brief well: state the goal, the files, the constraints ("do not change the tests"), and what
"done" means. Keep it small: the referee output carries the details on a return.

## Run and watch

```bash
upp validate match.toml                 # catches typos (unknown fields are errors)
upp run match.toml                      # foreground; one line per event; exit 0 if all scored
upp run match.toml --json               # quiet; final JSON summary
upp list                                # latest run: status, rallies, trail (. returned ! fault * point)
upp status fix-parser                   # rallies with referee output and notes (JSON)
upp export > rallies.jsonl              # trace
```

For a human watching: `upp watch` (live TUI). For long runs: start `uppd` and queue with
`upp create match.toml`.

## Be the server (review every ball)

Set `server = { mode = "external" }` on the table. Each answer then waits for you:

```bash
upp pending                                   # ball, answer, referee checks (JSON)
upp answer fix-parser return "Handle week 53; the failing case is 2026-W53-7."
upp answer fix-parser accept                  # scores only if the referee passed
upp answer fix-parser stop
```

Write returns as one precise correction or the next step. Never accept a ball whose referee
failed (it is refused anyway).

## MCP

If the `unumpingpong` MCP server is configured (`upp mcp`), use the tools instead of the CLI:
`table_create` (`match` object or `file`), `table_wait` (blocks until a ball waits or tables end;
prefer it to polling), `table_status`, `table_list`, `ball_answer`, `table_stop`. Use absolute
`cwd` paths.

## Results

- `point`: `result` holds the accepted answer; `{{prev.answer}}` passes it downstream.
- `lost`: max rallies without a point. Read the last referee output in `upp status`; re-brief or
  switch player.
- `expired`: wall/token/cost lease used up. `skipped`: an upstream table did not score.
- `error`: setup failed, bad cwd, or the player could not start (see `reason`).
