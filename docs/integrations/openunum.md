# OpenUnum

[OpenUnum](https://github.com/corpunum/openunum) is an autonomous local agent framework.

## As a player

```toml
[table.player]
kind = "openunum"
base_url = "http://127.0.0.1:18880"    # default
password_env = "OPENUNUM_PASSWORD"     # only if the WebUI has a password (sent as X-OpenUnum-Password)
session_prefix = "upp-"                # OpenUnum sessionId = prefix + table id
timeout_secs = 1800
```

The session-tree fields `parent_session`, `model` and `thinking` are described below.

Each rally is one OpenUnum *turn* with a `turnId` chosen by UnumPingPong, and only that
turn's reply is accepted:

1. `GET /api/chat/pending?sessionId=upp-<table>`: if the session still runs a turn (an earlier
   rally that timed out), wait for it to end and discard its result.
2. `POST /api/chat {"sessionId": "upp-<table>", "message": "<ball>", "turnId": "<uuid>"}`
   answers `200` with the finished turn, or `202 {pending: true, turnId}` while it runs. A
   response whose `turnId` is not ours (OpenUnum queued the message into a turn that was
   already running: `queued: true`, `message_queued_for_current_turn`) is a **fault**: its
   reply would belong to the previous turn.
3. On `202`, poll `GET /api/chat/pending?sessionId=...` every 1.5 s until a completed payload
   with our `turnId` arrives; completed payloads of other turns are ignored.

A terminal payload with `ok: false` (`chat_turn_failed`), `cancelled: true`,
`timedOut`/`timeout: true` (`chat_hard_timeout`), or an empty `reply` is a fault, never an
answer, so it cannot reach the referee as if the agent had answered.

The session persists across rallies, so the agent keeps its context, memory and tools. The
referee still runs outside OpenUnum: an OpenUnum agent cannot score its own point.

### Session-tree mode

With `parent_session` (or `$UPP_OPENUNUM_PARENT`) each table's player is a *worker* of that
OpenUnum session, so OpenUnum's budgets, guards, memory and UI apply:

```toml
[table.player]
kind = "openunum"
parent_session = "my-orchestrator-session"
model = "luna"          # the worker's model (optional)
thinking = "high"       # its thinking level (optional; the table's `effort` is the fallback)
```

Rally 1 calls `session_spawn` under the parent (`POST /api/tool/run`) with
`observationRoots: [<table cwd>]` and a referee-owned wall-time budget, plus
`implementationWorktree: <cwd>` only when the cwd is a git top level (OpenUnum refuses it
elsewhere). Later rallies resume the same worker (`POST /api/sessions/<id>/resume`); its result
summary is the answer. A stopped table cancels the worker.

**Status: run live (2026-10-20).** Two parallel tables with `kind = "openunum"` against a live
OpenUnum (local Qwen 3.6 35B via Halogen), each a small Python task checked by a `python3`
referee: **2/2 points on rally 1** (slugify 116 s, fib 437 s on a shared, busy model endpoint).
The turn ids lined up and no reply from another turn was accepted. Also tested against a scripted
HTTP fake (busy session, queued 202, another turn's late result, cancelled, timed out).
Known gap: OpenUnum's chat reply does not report token usage, so these tables show 0 tokens.

## As a server

OpenUnum can drive tables through MCP (`upp mcp`, see [generic-mcp.md](generic-mcp.md)) or the
HTTP API of `uppd` (`POST /api/tables`, `GET /api/wait`, `POST /api/tables/{id}/answer`), e.g.
from a skill. With `server.mode = "external"`, OpenUnum's orchestrator reviews every ball with
the referee evidence attached.

### OpenUnum's `pingpong_*` tools

`uppd` serves OpenUnum's own table API, same paths and shapes: `GET`/`POST
/api/pingpong/tables`, `GET /api/pingpong/tables/{id}`, `POST .../{id}/answer` (`verdict`:
`return`, `accept` or `stop`, plus `note`) and `POST .../{id}/stop`. An OpenUnum configured to
forward to the daemon (`runtime.pingpong.daemonUrl`, set on the OpenUnum side) therefore runs
its tables here. Those tables use the player from `uppd --compat-player '<player JSON>'` (or
`$UPPD_COMPAT_PLAYER`), by default `claude`; for example `{"kind":"openunum"}`. Off loopback the
daemon requires its bearer token (see `uppd --help`).
