# Changelog

## v0.4.0 (2026-10-21) - Merge-up, clean benchmark, harness comparison

- **Merge-up of sub-tables.** A writer sub-table that scores is merged into its parent's
  worktree before the parent's referee runs (`merge_children`, default true), so every level
  of a nested run checks the combined work. A conflict is aborted cleanly and returns the
  ball to the parent with the files and the branch to merge.
- **Benchmark correction.** The agent sandbox mounted `node_modules` read-only, so sandboxed
  agents (Codex CLI, opencode, nanocode) could not run the tests; single sessions were
  handicapped and some v0.3.0 claims were wrong. Fixed with an overlay mount; every affected
  player was re-run and the old runs are listed as superseded.
- **Same model, three harnesses**: GPT Luna (low) in Codex CLI, nanocode and OpenUnum's agent
  loop. Codex CLI tables are the cheapest 10/10. The comparison found a missing prompt-cache key
  in OpenUnum (fixed there: -55% tokens).
- Results now in three kinds: solves as much or more with fewer tokens; solves more at a higher
  cost; no gain yet (with a "help wanted" section in the README).
- Bench: `nanocode` player (built from source, sandboxed), `--model` for the Codex player,
  token counts for the OpenUnum player from OpenUnum's own log (`openunum_tokens.py`).
- Docs: table worktrees as an UnumSearch split root (new worktrees searchable within a second).

## v0.3.0 (2026-10-21) - Real-repository benchmark, shared sessions, sandboxed agents

UnumPingPong was measured on 10 real bug fixes from a production repository, with nine
player setups: Claude Haiku, GPT Luna (Codex CLI, and inside the OpenUnum agent loop), local
Qwen 3.6 35B, free opencode and NVIDIA NIM models, and small local models on a ROG Ally and a
Galaxy S22. Results: [bench/repo/results.md](bench/repo/results.md).

- **`share = "<key>"` (tables and defaults).** Tables with the same key take turns in one
  long-lived player session instead of one session each. Each ball is prefixed with that
  table's working directory. An option, not a default: in our runs it did not pay off for
  Codex (most tokens of any arm).
- **Players see their table directory in `$PWD`.** Spawned players got the caller's `$PWD`,
  and tools that trust it (opencode) worked in the wrong folder; one benchmark agent left its
  task folder that way. `PWD` is now set on every spawn and opencode also gets `--dir`.
- **`UPP_PLAYER_TRACE=<file>`** appends every raw message a persistent agent CLI sends, to
  audit token accounting against the tool's own counters.
- **Benchmark harness `bench/repo`**: tasks built with `git archive` (no history to look the
  fix up), referee scripts kept outside the agent's checkout, arms A (one session),
  B (tables), C (shared session), and `agent-sandbox.sh`: a rootless podman sandbox that lets
  opencode or Codex write only the task folder.
- **README rewritten** around the idea and the measured results; full option reference in
  [docs/REFERENCE.md](docs/REFERENCE.md), earlier synthetic benchmarks in
  [docs/BENCHMARKS.md](docs/BENCHMARKS.md).
- For Codex players, a short `system_prompt` cut table token use by 34% at the same 10/10.

## v0.2.2 (2026-10-20) - Reliability hardening (review follow-up)

An external review of v0.2.1 found four gaps (a fail-open orchestrator fallback, blind replay
after a crash, shared child working directories, an unauthenticated daemon); all four were
confirmed in the code and are fixed here. The OpenUnum player also ran live for the first
time: 2/2 points on rally 1, in parallel.

- **Orchestrator review policy (fail closed).** `[orchestrator] review = "referee"` (default,
  unchanged): a card with no valid decision falls back to the referee. `review = "required"`:
  a missing or invalid decision never scores a point. The card is asked again on the next
  batch, and after 3 tries the table is stopped with a `needs_review` event and listed under
  `needs_review` in the orchestrator's result, so the run ends instead of waiting on it.
- **Writer children get their own worktree.** Sub-tables (`create --parent`, `open`, `next`)
  of a table that runs in a git worktree each get their own worktree, branched from the
  parent's branch, so parallel children never edit the same files. `read_only = true`
  children share the parent's working directory; if a writer cannot be isolated, creating
  it is refused.

- **Durable recovery.** Before each player call the table records the rally it started
  and the git state it started from. If the scheduler dies mid-rally, `upp resume` never
  replays it blind: the player is told what changed since it started (untracked files
  included) and to continue from the current state (`recovered_inflight` event).
- **The wall lease survives resumes.** Time used before a crash counts against the lease,
  so a resumed table cannot run longer than its lease in total.

- **uppd authentication.** Any non-loopback `--listen` requires a bearer token; `--auth`
  requires it on loopback too. The token comes from `UPPD_TOKEN`, `--token-file`, or
  `$XDG_CONFIG_HOME/upp/token` (created 0600; only its path is printed). `/health` stays open.
- **Machine-wide parallel cap.** `UPP_GLOBAL_PARALLEL=N` / `uppd --global-parallel N`: at most
  N tables running across every process sharing the store, checked atomically at claim time.

## v0.2.1 (2026-10-19) - Token carry for agent CLI players

With Claude Code and Codex as players, v0.2.0's tables used 1.3-2.8x more tokens than one
agent session: every rally was a fresh CLI invocation that re-sent the CLI's system prompt and
tool schemas, and the accounting mixed cache reads with fresh input.

- **Persistent players.** `claude` and `codex` players keep one process per table (`persistent`,
  default true) and send each rally as a new turn: Claude Code over
  `--input-format stream-json --output-format stream-json`, Codex over `codex app-server`
  (JSON-RPC: `thread/start`, then one `turn/start` per rally, effort per turn). Claude restarts
  with `--resume` when the rally effort changes (`--effort` is per process). When the mode is not
  available (the process cannot start or speak the protocol, or Codex `args` are exec-only) the
  table falls back to the per-rally resume path; a `player_mode` event records which ran.
- **Delta-only returns.** A returned ball to a player that keeps its session carries only the
  failing checks with the head (700 B) and tail (1.3 KB) of their output, any server note, and
  one line asking for the fix; the brief is never repeated (players without a session still get
  it). Fault messages are trimmed the same way.
- **Lean CLI context** (`lean`, default true for claude/codex). Claude: `--strict-mcp-config`,
  `--setting-sources ""`, `--disable-slash-commands`, `--exclude-dynamic-system-prompt-sections`.
  Codex: extra feature tools, memories, hooks and web search off, and every MCP server from the
  user's codex config turned off by name (`-c mcp_servers={}`, used by the v0.2.0 benchmark, did
  not turn them off). New per-player `tools` (Claude `--tools`; `[]` = none, e.g. when `apply`
  writes the answer; Codex `[]` turns its shell tools off) and `system_prompt` (replaces the
  CLI's own: Claude `--system-prompt`, Codex base instructions).
- **Honest accounting.** Every rally records fresh input (`tokens_in`, cache writes included),
  cache reads (`tokens_cached`, new column) and output (`tokens_out`) apart, plus the CLI's
  reported cost (Claude's cumulative stream cost becomes a per-rally difference). Codex, Gemini
  and OpenAI-compatible `input`/`prompt` counts include the cached part and are split. Table
  `tokens`, cards and `max_tokens` leases count **billable tokens**: fresh input + output + cache
  reads at 0.1. `upp export` / transcripts carry `tokens_cached` and `billable_tokens`.
- **UnumSearch for players** (off by default). `search = "unumsearch"` on a `claude` or `codex`
  player attaches only the UnumSearch MCP server (Claude: `--mcp-config` + `--strict-mcp-config`
  + `--allowedTools` for its three read-only tools; Codex: `-c mcp_servers.unumsearch.*`, other
  servers stay off). For chat players without tools: `{{search:query}}` in a ball and a table's
  `context_search = [...]` run `unumsearch search` in the table's cwd before rally 1 and put the
  top 8 hits (`path:line: text`, capped) into the brief. Tested with a fake `unumsearch`;
  verified live with Haiku and Luna (both called the search tool and found the right line).
- Claude Code player forwards the per-rally `effort` as `--effort` (it was ignored for Claude).
- **Benchmark.** `bench.py --player claude|codex` (arm A one CLI session, arm B tables), now with
  the same lean flags for both arms, billable tokens as the headline, `--b-persistent`,
  `--b-lean`, `--b-tools`, `--b-system-prompt` and `--arm-key` for ablations. Re-run with Haiku
  and Luna-low: tables now use 0.43-0.63x the billable tokens of one session (Haiku hard 41k vs
  86k, $0.0146 vs $0.0203; Luna hard 33k vs 77k, 8/8 vs 7/8), except Luna on the easy set
  (1.09x: six small tables each pay an uncached first call). The persistent process alone saves
  no tokens (Haiku hard ablation: 121k -> 130k); lean context, no player tools and a short
  system prompt do (-> 92k -> 62k -> 41k). Results, card and README updated.

## v0.2.0 (2026-10-19) - Cards and a persistent orchestrator

The model to copy is the session that worked: one long-lived orchestrator reading compact
results from many workers and answering each at once. v0.2 makes that the mechanism.

- **Result cards.** Every rally is condensed to a card: `done` (the player's claim),
  `summary`, `evidence`, `open_issues`, `artifacts`, tokens, elapsed time, the referee verdict
  with an 800-byte output tail, provenance, sub-table results. The player is asked for a
  `<card>` tail block; a deterministic extractor covers answers without one. Cards are stored
  next to the full transcript (new `cards` table; a `final` card closes each table).
  `table_status`, pending balls, the model server and every orchestrator get cards by default;
  `transcript` (MCP tool, `upp transcript`, `GET /api/tables/{id}/transcript`) fetches the full
  text on demand, including the rally waiting for its server. Per-table token split:
  `player_tokens` vs `orchestrator_tokens` (a model server's or orchestrator's reported usage;
  for an external server an estimate of what it read and wrote).
- **Persistent orchestrator.** `[orchestrator] player = {...}` (any adapter) serves every card of
  a run as it arrives: accept, return with a correction, chain `next`, `open` a table, or stop,
  in batches, never blocking on one table. It falls back to the referee's default when it
  fails or skips a card, refreshes its session every 40 cards, and caps new tables (50).
  `upp orchestrate` runs one across runs. MCP `cards_wait` (block until any card in scope needs
  an answer; cursor for the stream) + `card_answer` (with `next` / `open`) let a Claude Code
  session be that orchestrator; the recipe is in docs/integrations/claude-code.md.
- **Event-driven waits.** A condition variable wakes waiters in one process at once; other
  processes' writes are seen through SQLite `data_version` within 0.4 s. The scheduler, external
  server waits, `table_wait`, `cards_wait` and the orchestrator no longer poll queries.
- **Nested tables.** A player opens sub-tables with its table as `parent` (MCP `table_create`
  `parent`, `upp create --parent`, `POST /api/tables?parent=`). Players get `UPP_TABLE`,
  `UPP_RALLY`, `UPP_RUN`, `UPP_DEPTH`, `UPP_DB`. Sub-tables inherit player and cwd, need their
  own referee; the parent's rally waits for them without holding a parallel slot; their final
  cards appear in the parent's card. Depth cap `max_depth` (default 3). Stopping a table stops
  its sub-tables.
- **Worktrees and integration.** `worktree = { repo, base, branch, path }` runs a table in its own
  git worktree and branch. A point commits exactly the verified tree on that branch.
  `upp integrate` / MCP `integrate` / `POST /api/integrate` merges a run's verified branches into
  `upp/integrate-<run>` in order, stops at the first conflict, and runs `--check`.
- **Provenance.** Each rally records `{commit, tree, dirty, changed, scope}`: the git tree the
  referee checked, written through a temporary index (the user's index is untouched): every
  non-ignored file in a table's own worktree, tracked files elsewhere.
- **Real cancellation.** A stop kills the running player call's process group (HTTP players
  abandon the request; OpenUnum turns are cancelled). Ctrl-C on `upp run` stops and kills too.
- **Crash recovery.** Tables of a dead scheduler process are requeued (`resumes` counter), not
  marked stopped; `upp resume` or `uppd` continues each after its last recorded rally, restoring
  agent CLI sessions by id and OpenAI-style conversations from the recorded rallies.
- **Leases are guards.** Defaults are now 2 hours and 10 rallies (were 30 minutes and 5); they
  only stop runaways and never drive scheduling. Documented as such.
- **Effort hints.** `effort = "low"` or a ladder `["low", "medium", "high"]` per rally; sent as
  `reasoning_effort` by the `openai` player (which also gains a `reasoning_effort` default),
  `-c model_reasoning_effort` for codex, `UPP_EFFORT` for shell players.
- **`--parallel auto`** / `max_parallel = "auto"`: the sum of the slots the tables'
  OpenAI-compatible endpoints report (`/props`, `/health`, `/slots`), minus the requests they
  are already serving (a shared endpoint is not crowded out), 4 otherwise.
- **Benchmark.** A harder 8-task set (bench/tasks-hard), arm C (tables + a persistent
  orchestrator on the same model), headline metric verified tasks per minute and per 100k
  tokens, and the wall-time investigation: see bench/results.md.
- **Store migration.** v0.1 databases are upgraded in place (new columns and the `cards` table).
- **Behaviour changes.** The external server payload carries `card` and referee output instead of
  `ball`/`answer` (set `server.transcript = true` for the old fields). `table_status` lists cards
  per rally; the answer is in `--full` or `transcript`. `result` no longer contains the card block.

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
