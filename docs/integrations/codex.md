# OpenAI Codex CLI

## As a player

```toml
[table.player]
kind = "codex"
# model = "..."                                          # optional: -m
args = ["-c", "sandbox_mode=\"workspace-write\""]          # must be valid for `exec` and `exec resume`
timeout_secs = 1200
```

Rally 1:

```bash
codex exec --json --skip-git-repo-check -o <tmpfile> [-m M] <args...> -        # ball on stdin
```

Later rallies resume the same thread:

```bash
codex exec resume --json --skip-git-repo-check -o <tmpfile> [-m M] <args...> <thread_id> -
```

The thread id comes from the `thread.started` event, tokens from `turn.completed.usage`, the
answer from the `-o/--output-last-message` file (fallback: the last `agent_message` item).
`turn.failed` / `error` events become a fault, even when an agent message (or the `-o` file)
was produced first. A nonzero exit status is a fault for every agent CLI (claude, codex,
gemini, opencode), whatever it printed.

**Status: flags verified** against codex-cli 0.160.0 (`codex exec --help`,
`codex exec resume --help`); event names (`thread.started`, `turn.completed`, `item.completed`,
`agent_message`) checked in the binary. **Verified live** (2026-10-19, `-m gpt-5.6-luna`, `-c model_reasoning_effort="low"`): multi-rally benchmark runs resumed the thread (same thread id across rallies) and parsed `turn.completed` usage; Codex reports no cost. See `bench/results.md`.

Caveat: `codex exec resume` accepts fewer options than `codex exec` (no `-s/--sandbox`, no
`-C`). Pass sandbox settings as `-c sandbox_mode="..."`, which both accept, or
`--dangerously-bypass-approvals-and-sandbox` in a disposable checkout. The table's `cwd` is the
working directory (no `-C` needed).

## As a server over MCP

```bash
codex mcp add unumpingpong -- upp mcp
codex mcp list
```

or in `~/.codex/config.toml`:

```toml
[mcp_servers.unumpingpong]
command = "upp"
args = ["mcp"]
```

**Status: docs** (`codex mcp add --help` checked; not registered on the test machine to leave its
global config untouched).

Codex also reads Agent Skills: copy `skills/unumpingpong/` into `~/.agents/skills/`.
