# Handover (living document)

Update this file with every change that alters state, open work or how to ship. Newest state on top.

## State (2026-11-12)

- Released: **v0.5.0** (tag 2026-11-12 on 11eafa9; `release.yml` builds the binaries). Installed
  `uppd` = 11eafa9 (`/health` git_sha), previous binary kept as `~/.local/bin/uppd.prev-e05c1f6`.
- Workspace: `crates/upp-core` (library), `crates/upp` (CLI + TUI + MCP), `crates/uppd` (daemon,
  default `127.0.0.1:7878`). `crates/uppd/build.rs` embeds the build commit as `UNUM_GIT_SHA`.
- CI: one Linux job (`fmt`, `clippy -D warnings`, all tests, example validation). `release.yml`
  runs on a `v*` tag. Pushes go through `unumship request --repo unumpingpong`.
- `~/.config/uppd/env` sets `UPPD_COMPAT_PLAYER` (openunum player, model luna, parent = OpenUnum
  work-log session). A table's `players[0].model` overrides it, e.g. the local main model
  `llama-cpp-local/openai/qwen3.8-flash-next-uncensored-lean`.
- OpenUnum still runs its own JS pingpong runner (`runtime.pingpong.daemonUrl` empty). Making `uppd`
  the engine is corpunum-82's decision after the parity rally (hub U-3061e807).
- False "execution contract not observed" warning on player replies: hub U-70323beb. Fixed in
  OpenUnum 3056e768 (observationRoots) + ef250f14 (per-root snapshots; the live workspace ~/main is
  too deep for one shared walk). The uppd player sends `observationRoots: [cwd]`.
- `leaseMs` means a total wall budget in both runners (set once at start).
- Use `~/.cargo/bin/cargo` (1.99): `/usr/bin/cargo` 1.75 cannot read the v4 `Cargo.lock`.

## How to ship

1. Detached worktree of `origin/main`, change, then the gates:
   `cargo fmt --all -- --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --all-targets --locked`
   and `for f in examples/*.toml examples/demo/*.toml; do cargo run -q --bin upp -- validate "$f"; done`.
2. One push to `main`; wait for CI to end green.
3. Deploy the daemon only when no table is running (`GET /api/tables`): build `--release`, replace
   the installed `uppd`, restart the unit, check `/health` shows the new `git_sha`.

## Open work

- Benchmark refresh: `bench/` has the 12 real bug fixes; rerun when a player or model changes and
  update `docs/BENCHMARKS.md`.
- Integrations to verify end to end after the release: `docs/integrations/` (Claude Code, Codex,
  Gemini CLI, opencode, OpenAI-compatible, OpenUnum).

## Traps

- Integration tests use scripted fake agents (`tests/fake-agents`); no model is needed. They start
  real `uppd` processes on free ports, so a firewall or a sandbox without loopback breaks them.
- `/health` is open by design; every other route needs the token when one is configured.
