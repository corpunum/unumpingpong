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
`is_error: true` or a nonzero exit is recorded as a fault and the ball is served again. A stop
kills the `claude` process group at once.

**Status: flags verified** against Claude Code 2.1.303 `--help` (`-p/--print`,
`--output-format json`, `--resume`, `--model`, `--permission-mode`); the result parser is tested
on the documented result object. No paid model turn was run while building v0.1.

Permissions: in `-p` mode Claude Code cannot ask you. Without an edit permission it answers but
does not change files. Pick one: `--permission-mode acceptEdits`, `--allowedTools "Edit Bash(cargo test:*)"`,
or (only in a disposable checkout) `--dangerously-skip-permissions`.

## As the orchestrator over MCP (cards_wait / card_answer)

```bash
claude mcp add unumpingpong -- upp mcp                     # this project, only you
claude mcp add --scope user unumpingpong -- upp mcp        # every project
claude mcp get unumpingpong                                # shows "✔ Connected"
```

**Status: registered** (Claude Code 2.1.303 reported `✔ Connected`). The tools show up as
`mcp__unumpingpong__<tool>`.

This is the workflow that worked by hand, made explicit. One long-lived Claude Code session is
the orchestrator. Players (local models, Codex, other Claude sessions, OpenUnum) do the work at
many tables. Claude reads only **result cards** and referee output, and it answers each card as
it arrives, never waiting idle on one table:

1. Open tables with `server: {mode: "external"}` (so every card waits for you) and a real
   referee:

   ```json
   table_create {"match": {"name": "fix-parser", "cwd": "/abs/repo",
     "goal": "cargo test -p parser passes", "ball": "...brief: goal, files, constraints, done = ...",
     "worktree": {"repo": "/abs/repo"},
     "player": {"kind": "openai", "base_url": "http://127.0.0.1:8080/v1", "model": "local"},
     "effort": ["low", "medium", "high"],
     "referee": [{"kind": "command", "run": "cargo test -p parser", "timeout_secs": 600}],
     "server": {"mode": "external"}}}
   ```

2. Loop on the inbox. Call `cards_wait` without `since` first: it returns at once with every card
   that needs an answer, else it blocks until the next card arrives from **any** table. Keep the
   returned `cursor` and pass it as `since` next time to also see final cards.

3. Answer each card that has `needs_answer: true` immediately with `card_answer`:
   - referee passed and the summary meets the goal: `accept`, plus `next` to chain the next ball
     (it starts when this table scores and gets its result as `{{prev.answer}}`);
   - referee failed: `return` with a precise correction read from `referee.tail` (at most a few
     lines; do not solve it for the worker);
   - a new independent task appeared: `open` (a table object; player and cwd default to this
     table's);
   - going nowhere: `stop`.

4. Call `transcript(table, rally)` only when a card is not enough. It is the expensive read and
   is charged to the table's orchestrator-side token count (`table_status` shows
   `token_split: {player, orchestrator, cards_est, transcripts_est}`).

5. When the run's worktree tables have scored, `integrate {"run": "...", "check": "cargo test"}`
   merges exactly the verified trees into one branch, `upp/integrate-<run>`.

A prompt that sets this up:

> Use the unumpingpong MCP tools as the orchestrator. Open one external-server table per task
> below, each in its own worktree with `cargo test -p <crate>` as the referee. Then loop
> `cards_wait` -> `card_answer` until every table has ended: accept passing cards, return
> failing ones with one precise correction from the referee tail, and chain `next` balls for
> the follow-up steps. Read transcripts only when a card is unclear. Finish with `integrate`.

`upp mcp` runs queued tables itself. If `uppd` already runs them, use `upp mcp --queue-only`
so that this MCP server only queues tables and answers cards.

The same loop also runs without Claude, with any model as the orchestrator: add an
`[orchestrator]` section with a `player` to the match file (see the README), or run
`upp orchestrate file.toml`.

## Players that open sub-tables

A player's process gets `UPP_TABLE`, `UPP_RALLY`, `UPP_RUN`, `UPP_DEPTH` and `UPP_DB` in its
environment. A Claude Code player with the unumpingpong MCP server (or the CLI) can split its
ball:

```bash
upp create subtasks.toml --parent "$UPP_TABLE"     # or MCP table_create with "parent"
```

The sub-tables inherit the player and the working directory (the parent's worktree), unless
they set their own. They must state their own referee checks. The parent's rally waits until
they end before its referee runs, and their final cards appear in the parent's card under
`children`. Depth is capped by `max_depth` (default 3).

## Skill

```bash
mkdir -p ~/.claude/skills && cp -r skills/unumpingpong ~/.claude/skills/
```

Allow the CLI with `Bash(upp:*)`.
