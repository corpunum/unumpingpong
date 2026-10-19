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
`turn.failed` / `error` events become a fault.

**Status: flags verified** against codex-cli 0.160.0 (`codex exec --help`,
`codex exec resume --help`); event names (`thread.started`, `turn.completed`, `item.completed`,
`agent_message`) checked in the binary. Not run with a model while building v0.1.

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
