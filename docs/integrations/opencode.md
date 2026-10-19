# opencode

## As a player

```toml
[table.player]
kind = "opencode"
model = "opencode/nemotron-3-ultra-free"   # provider/model, optional: -m
args = ["--auto"]                          # auto-approve permissions (opencode's own flag)
timeout_secs = 900
```

```bash
opencode run --format json [-m provider/model] [-s <sessionID>] <args...> <ball>
```

The JSON events (`{type, timestamp, sessionID, part}`) give the session id, the answer (all
`text` parts), and tokens/cost from `step_finish` parts; `error` events become a fault. Rally 2+
passes `-s <sessionID>` to continue the same session. A ball that starts with `-` is prefixed
with `Task: ` so it is not read as a flag.

**Status: flags verified** against opencode 1.18.33 (`opencode run --help`); the event emitter
was read in the binary (`JSON.stringify({type, timestamp, sessionID, ...})` for `step_start`,
`step_finish`, `text`, `tool_use`, `reasoning`, `error`). Not run with a model.

## As a server over MCP

`opencode.json` in the project root (or `~/.config/opencode/opencode.json`):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "unumpingpong": { "type": "local", "command": ["upp", "mcp"], "enabled": true }
  }
}
```

**Status: registered** (`opencode mcp list` showed `✓ unumpingpong connected`, opencode 1.18.33).
