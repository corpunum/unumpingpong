# Gemini CLI

## As a player

```toml
[table.player]
kind = "gemini"
# model = "..."                    # optional: -m
args = ["--approval-mode", "auto_edit"]
timeout_secs = 900
```

Rally 1 picks a session UUID; later rallies resume it:

```bash
gemini -o json [-m M] --session-id <uuid> <args...> -p <ball>
gemini -o json [-m M] --resume <uuid>     <args...> -p <ball>
```

The JSON output (`{session_id, response, stats, error}`) gives the answer, the session id and
token counts (`stats.models.*.tokens.prompt/candidates`). Gemini CLI stores sessions per project
directory; the table's `cwd` is that directory, so resuming by UUID finds it. Set `resume = false`
to start fresh each rally (the return message then repeats the brief).

**Status: flags verified** against Gemini CLI 0.45.1 (`--help`), and the JSON shape and
`--resume <uuid>` handling were read in its bundled source (`JsonFormatter`,
`SessionSelector.findSession`: "a full UUID or an index number"). Not run with a model.

Note: the ball is passed as the `-p` argument, so a very large ball (over ~128 KB on Linux) fails;
keep briefs small (that is the point) or use `{{file:...}}` sparingly.

## As a server over MCP

```bash
gemini mcp add unumpingpong upp mcp                # project scope: .gemini/settings.json
gemini mcp add -s user unumpingpong upp mcp        # user scope
gemini mcp list
```

```json
{ "mcpServers": { "unumpingpong": { "command": "upp", "args": ["mcp"] } } }
```

**Status: registered** (0.45.1 wrote the entry and `gemini mcp list` listed it; project servers
stay disabled until the folder is trusted).
