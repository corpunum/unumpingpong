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

**Status: API checked** against OpenUnum's route handlers (`src/server/routes/chat_tools.mjs`:
`POST /api/chat` reads `body.turnId`; `GET /api/chat/pending`; the completed payload carries
`turnId`, see `src/server/services/chat_runtime.mjs`). Tested against a scripted HTTP fake
(busy session, queued 202, another turn's late result, cancelled, timed out). Not run against
a live OpenUnum (the owner's instance was left untouched).

## As a server

OpenUnum can drive tables through MCP (`upp mcp`, see [generic-mcp.md](generic-mcp.md)) or the
HTTP API of `uppd` (`POST /api/tables`, `GET /api/wait`, `POST /api/tables/{id}/answer`), e.g.
from a skill. With `server.mode = "external"`, OpenUnum's orchestrator reviews every ball with
the referee evidence attached.
