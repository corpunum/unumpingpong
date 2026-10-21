<div align="center">

# 🏓 UnumPingPong

**AI agents play ping-pong with your tasks. A referee decides who scores.**

[![CI](https://github.com/corpunum/unumpingpong/actions/workflows/ci.yml/badge.svg)](https://github.com/corpunum/unumpingpong/actions/workflows/ci.yml) [![release](https://img.shields.io/github/v/release/corpunum/unumpingpong?color=30a46c)](https://github.com/corpunum/unumpingpong/releases) [![license](https://img.shields.io/badge/license-Apache--2.0-3e63dd)](LICENSE) ![Rust](https://img.shields.io/badge/engine-Rust-e5484d)

<img src="docs/media/hero.svg" alt="A server serves a task, an AI player hits back an answer, the referee runs the real tests: a failure is returned with the exact error, a pass scores a point" width="900">

</div>

## The idea in 30 seconds

You give an AI agent a task. It says *"done!"*. Is it?

UnumPingPong never takes the agent's word for it. Every task is a **ball** served to a **player**
(Claude Code, Codex, opencode, a local model, any command). When the player hits an answer back,
an independent **referee** checks it with the real thing: your tests, a file, an endpoint.
**Fail?** The ball goes straight back, with the exact error, to the same player. **Pass?**
The table scores a **point**. Only verified work scores.

Run one table, or a whole hall of them:

<img src="docs/media/tables.svg" alt="Four tables rallying in parallel, and a serial chain plan → build → test where each step lights up only after its referee passed" width="900">

## Does it work? Real repo, real bugs

We took **10 real bug fixes** from our own agent framework (OpenUnum, a private repo; the harness
works on any repository), rewound each to just before the fix, and handed every agent the same
issue title plus the failing test, but **not** the file to change. A task counts only if its real unit test passes
afterwards and the test was not touched.

<img src="docs/media/results-repo.svg" alt="Benchmark chart: wall time and verified tasks per player, one agent session vs UnumPingPong tables" width="900">

What we learned (one run per cell unless noted: a first signal, not a law). Every result is one
of three kinds, and we show all three:

**✅ Solves as much or more, with fewer tokens**

- **Claude Haiku** (Claude Code), two runs: 10/10 either way, tables used **~40% fewer tokens** and
  were **3-8x faster** (one long session drags every earlier task along; small tables stay focused).
- **Nemotron 3 Ultra** (free on NVIDIA NIM): 10/10 either way, tables used **28% fewer tokens**
  (but took 29 vs 19 minutes).
- **GPT Luna** in the **nanocode** harness: 10/10 either way, tables 7% fewer tokens and faster.

**🟡 Solves more, at a higher token cost (better solutions)**

- **GPT Luna** in the **Codex CLI**: one session 8/10, tables **10/10** in less time, for 50% more
  tokens (215k -> 322k). The cheapest way we found to get 10/10 from Luna.
- **local Qwen3.8 Flash Next** (OpenUnum): 7/10 -> **9/10**, but 4x the wall time on one local GPU.
- **gpt-oss-20b** (free on NVIDIA NIM): **1/10 -> 6/10**, but at ~18x the tokens and 2.3 hours.

**The harness matters as much as the model.** The same model (GPT Luna, low effort) at tables:
Codex CLI 322k tokens, OpenUnum agent loop 738k, nanocode 1.7M, all 10/10. Measuring this also
found a bug in OpenUnum: it never sent Codex a prompt-cache key; the fix cut its Luna tokens by
55% (1.63M -> 738k).

So the recommended setup depends on the player: tables for Claude Code and Codex, and for open
models that cannot finish alone; a single session is fine for a strong model that already solves
everything. Every number, including the superseded runs, is in
[bench/repo/results.md](bench/repo/results.md).

### Where it does not help (yet): ideas and contributions welcome

We publish the misses too. If you have an idea for any of these, please
[open an issue](https://github.com/corpunum/unumpingpong/issues) or a PR:

- **Strong models that already solve everything alone** (Haiku, Nemotron, Luna in nanocode) gain
  only tokens and time, not solved tasks; through opencode's free tier, Nemotron gained nothing
  at all (10/10 both ways, same tokens, tables 2.7x slower). Can a cheaper first rally (e.g. a smaller model at
  rally 1, the strong one only after a failure) make tables pay off here too?
- **Weak models get expensive.** gpt-oss-20b solves 6/10 at tables, but each failed rally resends a
  growing context: 3.1M tokens. Smarter return balls (only the failing assertion, a diff of what
  changed) could cut that.
- **Codex CLI tables cost more tokens** than one long Codex session (322k vs 215k): Codex caches a
  long session cheaply and each table starts fresh. `share` (tables taking turns in one session)
  was built for this; its earlier measurement was invalid (see below) and needs a re-run.
- **Small on-device models** (SuperGemma 4 on a ROG Ally, Gemma-4 E2B on a phone) call tools but
  did not fix these bugs; their runs predate the sandbox fix below and need a re-run.

*A correction:* runs made before 2026-10-21 in our agent sandbox could not run the tests
(`node_modules` was mounted read-only), which handicapped single sessions; v0.3.0 claims like
"0/10 -> 7/10" came from that. Those runs are listed as superseded and every number above is
from the fixed sandbox.

The harness is in [bench/repo](bench/repo) (`python3 bench/repo/repo_bench.py --player …`);
older synthetic task sets are in [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

## Why a referee changes everything

| Without a referee | With UnumPingPong |
|---|---|
| "Done!" means the agent *thinks* it is done | done means **your tests pass** |
| failures hide in a long transcript | a failure comes back as a precise ball: *this test, this error* |
| one agent, one task at a time | many **tables** in parallel, or **chains** (plan → build → test) |
| you read thousands of lines | you read one **card** per table: done, evidence, open issues |
| a crash loses the session | every rally is in SQLite: replay, resume, audit, train on it |

## Try it in one minute

```bash
curl -fsSL https://github.com/corpunum/unumpingpong/releases/latest/download/install.sh | sh
upp --db /tmp/upp-demo.db run examples/demo/demo.toml --watch   # no model needed: scripted players, real referee
```

A real table is a few lines of TOML:

```toml
[[table]]
name    = "fix-tests"
ball    = "The tests fail. Fix the code, not the tests."
player  = { kind = "claude" }                       # or codex, gemini, opencode, openunum, openai, shell
referee = [{ kind = "command", run = "cargo test" }]
lease   = { max_rallies = 5, wall_secs = 900 }      # a guard against runaways, nothing more
```

```bash
upp run fix-tests.toml --watch     # live TUI of every table
upp list                           # who scored, who lost, why
```

<details>
<summary><b>🏓 For engineers: the pings, in one table</b></summary>

| Term | What it is |
|---|---|
| **ball** | the brief a player receives (task, files, the referee's checks) |
| **rally** | player answers → referee checks → the ball is returned with the evidence, or scores |
| **ping** | every event of a rally: SQLite trace, SSE stream (`uppd`), MCP (`upp mcp`) |
| **point** | the referee passed: a verified result, tied to the exact git tree it checked |
| **card** | the compact result of a rally: `done`, `evidence`, `open_issues`, `artifacts`, tokens |
| **server** | who serves and reviews: the referee alone, a model, you, or a long-lived orchestrator over MCP |
| **lease** | wall time, rallies, tokens, cost: a runaway guard, never the scheduler |
| **share** | tables that take turns in one player session (an option for Codex-style cached sessions; being re-measured) |
| **merge-up** | a writer sub-table that scores is merged into its parent's worktree before the parent's referee runs; a conflict returns the ball |
| **worktree** | each coding table gets its own git worktree; children that write get their own too |

The engine is one Rust library with thin front-ends: CLI with a TUI (`upp`), daemon with an
HTTP/JSON API and Server-Sent Events (`uppd`, token-protected beyond localhost), MCP server
(`upp mcp`), and an Agent Skill. It has no GPU, model or vendor dependency of its own.

</details>

<details>
<summary><b>🧩 Plug in any agent</b></summary>

| Player | How it plays | Status |
|---|---|---|
| `claude` | Claude Code, one persistent stream-json process per table | run live |
| `codex` | Codex `app-server`, one thread per table | run live |
| `opencode` | `opencode run`, any provider/model (free tier, NVIDIA NIM, local) | run live |
| nanocode | `shell` player + [nanocode-player.py](bench/repo/nanocode-player.py) (one session per table, continued per rally) | run live (bench) |
| `openunum` | an OpenUnum agent session per table (our own framework) | run live |
| `gemini` | Gemini CLI | flags verified |
| `openai` | any OpenAI-compatible endpoint (llama.cpp, vLLM, Ollama, LM Studio, Halogen) | run live |
| `shell` | any command | tested |

Recipes per tool: [docs/integrations](docs/integrations/README.md).

</details>

<details>
<summary><b>🔒 Safety rails</b></summary>

- The referee is deterministic first (commands, files, HTTP, JSON); a model judge is never the player itself.
- `review = "required"`: with a mandatory reviewer, no point without a real review.
- Crash recovery never replays a half-finished rally blind; the wall lease survives restarts.
- `uppd` requires a bearer token on any non-loopback address; `UPP_GLOBAL_PARALLEL` caps all runs on one machine.

</details>

## Learn more

[Reference](docs/REFERENCE.md) (every option) · [Integrations](docs/integrations/README.md) ·
[Benchmarks](bench/repo/results.md) · [Changelog](CHANGELOG.md) · [Agent Skill](skills/unumpingpong/SKILL.md)

<sub>Apache-2.0 · built by [corpunum](https://github.com/corpunum) · sibling project:
[UnumSearch](https://github.com/corpunum/unumsearch) (fast local code search, pluggable into players)</sub>
