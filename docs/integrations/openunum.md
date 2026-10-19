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

Each rally posts the ball to the table's own OpenUnum session:

- `POST /api/chat {"sessionId": "upp-<table>", "message": "<ball>"}` answers `200` with `reply`
  when the turn finishes quickly, or `202 {pending: true}` while it runs;
- then `GET /api/chat/pending?sessionId=...` is polled every 1.5 s until the completed turn
  (`reply`) comes back.

The session persists across rallies, so the agent keeps its context, memory and tools. The
referee still runs outside OpenUnum: an OpenUnum agent cannot score its own point.

**Status: API checked** against OpenUnum's route handlers (`src/server/routes/chat_tools.mjs`:
`POST /api/chat`, `GET /api/chat/pending`). Not run against a live OpenUnum while building v0.1
(the owner's instance was left untouched).

Caveat: a ball sent while the session already runs a turn is queued by OpenUnum (`202`,
`message_queued_for_current_turn`); UnumPingPong then waits for that turn's reply.

## As a server

OpenUnum can drive tables through MCP (`upp mcp`, see [generic-mcp.md](generic-mcp.md)) or the
HTTP API of `uppd` (`POST /api/tables`, `GET /api/wait`, `POST /api/tables/{id}/answer`), e.g.
from a skill. With `server.mode = "external"`, OpenUnum's orchestrator reviews every ball with
the referee evidence attached.
