# OpenAI Codex CLI

## As a player

```toml
[table.player]
kind = "codex"
# model = "..."                                          # optional
args = ["--dangerously-bypass-approvals-and-sandbox"]    # or ["-c", "sandbox_mode=\"workspace-write\""]
timeout_secs = 1200
persistent = true                                        # default: one app server per table
lean = true                                              # default: extra feature tools, MCP, memory off
# tools = []                                             # turns codex's shell tools off
# system_prompt = "You are ..."                          # replaces codex's base instructions
```

**Persistent (default).** One `codex app-server` per table (JSON-RPC over stdio, the protocol
from `codex app-server generate-json-schema`), started in the table's `cwd`:

```text
codex app-server <lean -c ...> <-c/--enable/--disable from args>
-> initialize {clientInfo}            <- result;  -> initialized
-> thread/start {cwd, model, approvalPolicy, sandbox, baseInstructions}   (thread/resume {threadId} after a restart)
-> turn/start {threadId, input:[{type:"text", text:<ball>}], effort}       one per rally
<- item/completed (agentMessage) ... thread/tokenUsage/updated ... turn/completed
```

The effort ladder is sent per turn (`effort`), so the process never restarts for it. Args are
translated: `-c`/`--config`, `--enable`, `--disable`, `--strict-config` pass through;
`--dangerously-bypass-approvals-and-sandbox` becomes `approvalPolicy: "never"` +
`sandbox: "danger-full-access"`, `-s/--sandbox X` becomes `sandbox: X`, `--full-auto` becomes
`workspace-write`; without one, `approvalPolicy` is `never` and the sandbox is codex's config
default. Any other argument is exec-only: that player keeps the per-rally path. Approval
requests that still arrive are declined (nobody is there to approve them). A failed turn
(`turn/completed` with status `failed`, or an `error` notification that will not be retried) is
a fault; the next rally runs on the same thread. If the app server cannot start or initialize,
the table falls back to the per-rally path; the `player_mode` event records which mode ran.

**Per rally** (`persistent = false`, or the fallback). Rally 1:

```bash
codex exec --json --skip-git-repo-check -o <tmpfile> [-m M] <lean -c ...> <args...> -        # ball on stdin
```

Later rallies resume the same thread:

```bash
codex exec resume --json --skip-git-repo-check -o <tmpfile> [-m M] <lean -c ...> <args...> <thread_id> -
```

The thread id comes from the `thread.started` event, tokens from `turn.completed.usage`, the
answer from the `-o/--output-last-message` file (fallback: the last `agent_message` item).
`turn.failed` / `error` events become a fault, even when an agent message (or the `-o` file)
was produced first. A nonzero exit status is a fault for every agent CLI (claude, codex,
gemini, opencode), whatever it printed. A `system_prompt` is passed as
`-c model_instructions_file=<tmpfile>`.

**Lean** (`lean = true`, the default): `-c features.<f>=false` for apps, browser/computer use,
image generation, multi-agent, goals, plugins, skill search, tool suggestions, image viewing,
sleep, realtime, in-app browser, workspace dependencies, hooks and memories;
`-c web_search="disabled"`; and `-c mcp_servers.<name>.enabled=false` for every MCP server in
`$CODEX_HOME/config.toml` (else `~/.codex/config.toml`). Note: `-c mcp_servers={}` (what the
v0.2.0 benchmark passed) does **not** turn configured servers off; the app server still started
them. Measured with `gpt-5.6-luna` on this machine, one "reply hi" call: 12.9k input tokens by
default, 6.5k lean, 3.7k lean with a short `model_instructions_file`.

**Tokens.** Codex's `input_tokens` include the cached part: fresh input = input - cached,
cached input = `cached_input_tokens` (`cachedInputTokens` per model call on the app server),
output = `output_tokens`. Codex reports no cost. Tables, cards and `max_tokens` count billable
tokens: fresh input + output + cache reads at 0.1.

**Code search.** `search = "unumsearch"` (or a path to the binary) adds
`-c mcp_servers.unumsearch.command="unumsearch" -c mcp_servers.unumsearch.args=["mcp"]
-c mcp_servers.unumsearch.enabled=true`; lean mode still turns every other configured MCP server
off (and leaves `unumsearch` on). Verified live (`gpt-5.6-luna`, app server): the player called
`unumsearch/search` and answered with the right `path:line`.

**Status: verified** against codex-cli 0.160.0 (`codex exec --help`, `codex exec resume --help`,
`codex app-server --help`, `codex features list`, the generated app-server JSON schema).
**Verified live** (2026-10-19, `gpt-5.6-luna`): two turns on one app-server thread with per-turn
`effort`; the cloud benchmark ran with the persistent adapter (`bench/results.md`).

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
