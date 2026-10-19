# Integrations

UnumPingPong meets agent tools in two directions:

- **As a player (worker).** The tool answers balls at a table. One adapter per tool, all behind
  the same `Player` trait; each keeps the tool's own session across rallies where it can.
- **As a server (orchestrator).** The tool drives tables over MCP (`upp mcp`), HTTP (`uppd`) or
  the CLI: it opens tables for workers and, with `server.mode = "external"`, reviews each ball
  with `ball_answer`.

Status legend: **tested** = run end to end with the tool on Linux; **tested live** = a real model
turn through the adapter; **registered** = the tool accepted and connected/listed the MCP
server, no model turn; **flags verified** = every flag checked against the installed tool's
`--help`, output parsing checked against the tool's JSON formatter or documented shape, not run
with a model; **docs** = from documentation only.

| Tool | As player (`kind`) | Player status | As server (MCP) | Guide |
| --- | --- | --- | --- | --- |
| Any command | `shell` | tested (CI, scripted agents) | n/a | [main README](../../README.md#players) |
| OpenAI-compatible endpoints (llama.cpp, Halogen, vLLM, Ollama, LM Studio, cloud) | `openai` | tested live (Halogen 0.18, Qwen 3.8) | n/a | [openai-compatible.md](openai-compatible.md) |
| OpenUnum | `openunum` | API checked against OpenUnum source | MCP client: docs | [openunum.md](openunum.md) |
| Claude Code | `claude` | flags verified (2.1.303) | registered (`✔ Connected`) | [claude-code.md](claude-code.md) |
| OpenAI Codex CLI | `codex` | flags verified (0.160.0) | docs (`codex mcp add --help`) | [codex.md](codex.md) |
| Gemini CLI | `gemini` | flags verified (0.45.1) | registered (listed; needs a trusted folder) | [gemini-cli.md](gemini-cli.md) |
| opencode | `opencode` | flags verified (1.18.33) | registered (`connected`) | [opencode.md](opencode.md) |
| Any MCP client | n/a | n/a | protocol tested (integration test) | [generic-mcp.md](generic-mcp.md) |

Checked on 2026-10-19. CLI flags drift; if one breaks, `args` on the player adds or overrides
flags without a new release, and the `shell` player wraps anything.
