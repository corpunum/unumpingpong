# Claude Code

## As a player

```toml
[table.player]
kind = "claude"
model = "sonnet"                                   # optional: --model
args = ["--permission-mode", "acceptEdits"]        # Claude Code's own permission flags
timeout_secs = 900                                 # per rally (also bounded by the wall lease)
resume = true                                      # default
persistent = true                                  # default: one process per table
lean = true                                        # default: no MCP, settings, skills
# tools = []                                       # --tools; [] = none (apply writes the file)
# system_prompt = "You are ..."                    # --system-prompt (replaces Claude Code's)
```

**Persistent (default).** One process per table, in the table's `cwd`; every rally is a new
user message on its stdin:

```bash
claude -p --input-format stream-json --output-format stream-json --verbose \
       [--model M] [--effort E] [--resume <session_id>] <lean flags> [--tools T] [--system-prompt P] <args...>
# stdin, one line per rally: {"type":"user","message":{"role":"user","content":"<ball>"}}
```

Each rally ends at the `result` line: the answer (`result`), the `session_id`, `usage` and
`total_cost_usd` (cumulative per process; the adapter records the difference per rally).
`--effort` is per process, so a rally with a different effort (an effort ladder) restarts the
process with `--resume` on the same session; the conversation and the prompt cache carry over.
A process that dies, times out or is stopped is restarted with `--resume` on the next rally. If
the stream mode is unavailable (the process ends before printing anything), the table falls back
to the per-rally path below; the `player_mode` event records which mode ran.

**Per rally** (`persistent = false`, or the fallback): each rally runs

```bash
claude -p --output-format json [--model M] [--resume <session_id>] <lean flags> <args...>   # ball on stdin
```

**Lean flags** (`lean = true`, the default): `--strict-mcp-config` (no MCP servers),
`--setting-sources ""` (no user/project/local settings: hooks, permissions allowlists, env),
`--disable-slash-commands` (no skills), and `--exclude-dynamic-system-prompt-sections` (cwd and
git status move to the first message, so all tables share one cached system prompt; not with
`system_prompt`). A flag already in `args` is not added twice. Set `lean = false` when the player
needs your settings (e.g. an `env` that routes Claude Code through a proxy). Measured with
Haiku on this machine, one "reply hi" call: 15.3k input tokens with the default tool set,
3.6k with `tools = []`, 0.6k with `tools = []` and a one-line `system_prompt`.

**Tokens.** `usage.input_tokens + cache_creation_input_tokens` is recorded as fresh input,
`cache_read_input_tokens` as cached input, `output_tokens` as output, `total_cost_usd` as cost.
Tables, cards and `max_tokens` count *billable* tokens: fresh input + output + cache reads at
0.1. `is_error: true` or a nonzero exit is recorded as a fault and the ball is served again. A
stop kills the `claude` process group at once.

**Code search.** `search = "unumsearch"` attaches the UnumSearch MCP server, and only that one:
`--mcp-config '{"mcpServers":{"unumsearch":{"type":"stdio","command":"unumsearch","args":["mcp"]}}}'`,
`--strict-mcp-config` and `--allowedTools mcp__unumsearch__search,mcp__unumsearch__find_files,mcp__unumsearch__index_status`.
Verified live (Haiku, `tools = []`): the player called `mcp__unumsearch__search` and answered with
the right `path:line`. See "Using UnumSearch from players" in the README.

**Status: flags verified** against Claude Code 2.1.303 `--help` (`--input-format stream-json`,
`--output-format stream-json`, `--verbose`, `--resume`, `--effort`, `--tools`, `--system-prompt`,
`--strict-mcp-config`, `--setting-sources`, `--disable-slash-commands`,
`--exclude-dynamic-system-prompt-sections`). **Verified live** (2026-10-19, Haiku 5.5): two user
messages over one process's stdin answered in one session (per-turn `usage`, cumulative
`total_cost_usd`); the cloud benchmark ran with the persistent adapter (`bench/results.md`).

Permissions: in `-p` mode Claude Code cannot ask you. Without an edit permission it answers but
does not change files. Pick one: `--permission-mode acceptEdits`, `--allowedTools "Edit Bash(cargo test:*)"`,
or (only in a disposable checkout) `--dangerously-skip-permissions`. With `apply = { write_file
= ... }` the player needs no tools at all: `tools = []`.

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
