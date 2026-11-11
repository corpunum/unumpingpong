# Handover (living document)

Update this file with every change that alters state, open work or how to ship. Newest state on top.

## State (2026-11-11)

- Released: **v0.4.0** (2026-10-21). `main` carries unreleased work (see `CHANGELOG.md`
  "Unreleased"): auto-merge after a referee pass, the OpenUnum-compatible `/api/pingpong/*` API,
  the systemd user unit, the session-tree `openunum` player, and `uppd` `/health` with `git_sha`
  and `started_at`.
- Workspace: `crates/upp-core` (library), `crates/upp` (CLI + TUI + MCP), `crates/uppd` (daemon,
  default `127.0.0.1:7878`). `crates/uppd/build.rs` embeds the build commit as `UNUM_GIT_SHA`
  (environment override; `unknown` outside a git checkout).
- CI: one Linux job (`fmt`, `clippy -D warnings`, all tests, example validation). `release.yml`
  runs on a `v*` tag.

- OpenUnum still runs its own JS pingpong runner (`runtime.pingpong.daemonUrl` is empty), so live
  rallies do not reach `uppd` yet. Making `uppd` the engine is the orchestrator's decision after a
  parity rally (11-11).
- `leaseMs` means a total wall budget in both runners (set once at start). A table that expires
  quickly was given a small `leaseMs` by its caller.
- Use `~/.cargo/bin/cargo` (1.99): `/usr/bin/cargo` 1.75 cannot read the v4 `Cargo.lock`.

## How to ship

1. Detached worktree of `origin/main`, change, then the gates:
   `cargo fmt --all -- --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --all-targets --locked`
   and `for f in examples/*.toml examples/demo/*.toml; do cargo run -q --bin upp -- validate "$f"; done`.
2. One push to `main`; wait for CI to end green.
3. Deploy the daemon only when no table is running (`GET /api/tables`): build `--release`, replace
   the installed `uppd`, restart the unit, check `/health` shows the new `git_sha`.

## Open work

- Cut **v0.5.0** from what is on `main` (changelog is ready; tag, release notes, binaries).
- Benchmark refresh: `bench/` has the 12 real bug fixes; rerun when a player or model changes and
  update `docs/BENCHMARKS.md`.
- Integrations to verify end to end after the release: `docs/integrations/` (Claude Code, Codex,
  Gemini CLI, opencode, OpenAI-compatible, OpenUnum).

## Traps

- Integration tests use scripted fake agents (`tests/fake-agents`); no model is needed. They start
  real `uppd` processes on free ports, so a firewall or a sandbox without loopback breaks them.
- `/health` is open by design; every other route needs the token when one is configured.
