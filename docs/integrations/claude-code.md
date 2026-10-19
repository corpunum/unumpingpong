# Claude Code

## As a player

```toml
[table.player]
kind = "claude"
model = "sonnet"                                   # optional: --model
args = ["--permission-mode", "acceptEdits"]        # Claude Code's own permission flags
timeout_secs = 900                                 # per rally (also bounded by the wall lease)
resume = true                                      # default
```

Each rally runs, in the table's `cwd`:

```bash
claude -p --output-format json [--model M] [--resume <session_id>] <args...>   # ball on stdin
```

The JSON result gives the answer (`result`), the `session_id` (passed back with `--resume` on the
next rally, so Claude keeps its context and prompt cache), `usage` (input + cache + output
tokens, counted against `max_tokens`) and `total_cost_usd` (counted against `max_cost_usd`).
`is_error: true` is recorded as a fault and the ball is served again.

**Status: flags verified** against Claude Code 2.1.303 `--help` (`-p/--print`,
`--output-format json`, `--resume`, `--model`, `--permission-mode`); the result parser is tested
on the documented result object. No paid model turn was run while building v0.1.

Permissions: in `-p` mode Claude Code cannot ask you. Without an edit permission it answers but
does not change files. Pick one: `--permission-mode acceptEdits`, `--allowedTools "Edit Bash(cargo test:*)"`,
or (only in a disposable checkout) `--dangerously-skip-permissions`.

## As a server (orchestrator) over MCP

```bash
claude mcp add unumpingpong -- upp mcp                     # this project, only you
claude mcp add --scope user unumpingpong -- upp mcp        # every project
claude mcp get unumpingpong                                # shows "✔ Connected"
```

**Status: registered** (Claude Code 2.1.303 reported `✔ Connected`). Tools appear as
`mcp__unumpingpong__table_create`, `..._table_status`, `..._table_list`, `..._ball_answer`,
`..._table_stop`, `..._table_wait`.

Typical session: Claude creates tables for local or cheaper workers with
`server: {mode: "external"}`, then loops `table_wait` -> read `pending` (ball, answer, referee
checks) -> `ball_answer` with `return` + a precise note, or `accept` when the referee passed.

`upp mcp` runs queued tables itself. If `uppd` already runs them, use `upp mcp --queue-only` so
the MCP server only queues and answers.

## Skill

```bash
mkdir -p ~/.claude/skills && cp -r skills/unumpingpong ~/.claude/skills/
```

Allow the CLI with `Bash(upp:*)`.
