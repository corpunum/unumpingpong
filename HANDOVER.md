# Handover (living document)

Update this file with every change that alters state, open work or how to ship. Newest state on top.

## State (2026-11-12)

- Released: **v0.6.0** (2026-11-12; tagged with `unumship release --repo unumpingpong --tag v0.6.0`
  on the shipped-green head; `release.yml` builds the binaries). Previous: v0.5.0 on 11eafa9.
  `upp referee --fix` is in v0.6.0 but held: no automatic use until the fix player runs sandboxed
  (OpenUnum per-worker sandbox, hub U-4b3a6f59) and a narrow 4th review passes.
- Workspace: `crates/upp-core` (library), `crates/upp` (CLI + TUI + MCP), `crates/uppd` (daemon,
  default `127.0.0.1:7878`). `crates/uppd/build.rs` embeds the build commit as `UNUM_GIT_SHA`.
- CI: one Linux job (`fmt`, `clippy -D warnings`, all tests, example validation). `release.yml`
  runs on a `v*` tag. Pushes go through `unumship request --repo unumpingpong`.
- `~/.config/uppd/env` sets `UPPD_COMPAT_PLAYER` (openunum player, model luna, parent = OpenUnum
  work-log session). A table's `players[0].model` overrides it, e.g. the local main model
  `llama-cpp-local/openai/qwen3.8-flash-next-uncensored-lean`.
- False "execution contract not observed" warning on player replies: hub U-70323beb. Fixed in
  OpenUnum 3056e768 (observationRoots) + ef250f14 (per-root snapshots; the live workspace ~/main is
  too deep for one shared walk). The uppd player sends `observationRoots: [cwd]`.
- `leaseMs` means a total wall budget in both runners (set once at start).
- `upp referee --repo DIR --sha SHA -- <cmd>` referees a commit in a temp detached worktree (no
  model) and prints a JSON verdict (v1). unumship (0.8.12+) calls it for entries queued with
  `--referee '<cmd>'` (or a repo default `referee`): exit 1 → `referee_failed`, the entry stays out
  of the bundle; exit 2 → `referee_error`, it still ships. Hub U-80a29982. The light check for this
  repo: example validation with the installed `upp` plus `cargo fmt --check`
  (the full tests run in the unumship gate).
- OpenUnum routes `/api/pingpong/*` here (`runtime.pingpong.daemonUrl`, set by corpunum-82
  11-12 06:50Z; rollback through corpunum-82). The compat player has no model pin, so rallies
  default to OpenUnum's main local model (backup of the old env: `~/.config/uppd/env.bak-20261112`).
- Use `~/.cargo/bin/cargo` (1.99): `/usr/bin/cargo` 1.75 cannot read the v4 `Cargo.lock`.

## `upp referee` phase 2

- `upp referee --fix N --player JSON --patch-dir DIR --protect PATH`: after a failed check a
  player gets at most N rallies in its own isolated clone; a scoring fix is re-verified as a patch
  on a clean clone and written to `--patch-dir`. Verdict fields: `sandbox`, `net`, `sandboxProfile`,
  `fix{point,rallies,status,reason,escalate,patch,files,diffstat,verified}`.
- Sandbox profile 2 (`--sandbox auto|bwrap|none`, `--net`, `--ro PATH`): see
  `docs/REFERENCE.md` "Refereeing a commit". Checkouts hold the commit and its parent only
  (`--depth 2`); checks get `UPP_SHA`, `UPP_BASE_SHA`, `UPP_CHANGED_PATHS`.
- Security reviews: the first failed. The second passed check-only mode but failed the fix flow
  (template injection through the check command; the brief asked the player to run the check).
  The third: the fix flow passes with conditions: the player's sandbox must not reach the referee
  temp dir or the patch dir, and the verify clone must be created after the player. Both are
  addressed or required in this bundle (the clone is created after the table and its git config is
  compared; `fix.sha256` lets the applier detect a changed patch; a sandboxed player that binds
  only its own work dir is still required and does not exist yet). A fourth review of these fixes
  is pending.
- Automatic fix runs in the ship queue stay disabled until the per-worker sandbox exists.
- Closed for referee fix tables: the table engine's provenance git (`git::provenance`, `git add
  -A` in the player's directory, which uses the player's `.git/config`) no longer runs for them.
  `upp referee --fix` sets the table option `no_provenance`, so no git runs in the player's clone
  and provenance is `{"git": false}`; sub-tables inherit it. Ordinary tables still run provenance
  git in the player's directory with the player's git config (documented limit: run players
  sandboxed with a read-only `.git`, or set `no_provenance`).
- Verdict text (`fix.files`, `diffstat`, `reason`, `output`) is untrusted: fence it before a
  model reads it. Credential masking is a name heuristic to depth 2.

## How to ship

1. Detached worktree of `origin/main`, change, then the gates:
   `cargo fmt --all -- --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --all-targets --locked`
   and `for f in examples/*.toml examples/demo/*.toml; do cargo run -q --bin upp -- validate "$f"; done`.
2. One push to `main`; wait for CI to end green.
3. Release: bump `version` in Cargo.toml, rename "Unreleased" in CHANGELOG.md, ship that commit,
   then tag it with `unumship release --repo unumpingpong --tag vX.Y.Z` (never a raw tag push).
4. Deploy the daemon only when no table is running (`GET /api/tables`): build `--release`, replace
   the installed `uppd`, restart the unit, check `/health` shows the new `git_sha`.

## Open work

- Benchmark refresh: `bench/` has the 12 real bug fixes; rerun when a player or model changes and
  update `docs/BENCHMARKS.md`.
- Integrations: verified end to end on 2026-11-12 (`docs/integrations/`, "Verified" line in each
  guide): live rallies for Claude Code, Codex, opencode and OpenUnum; `upp mcp` handshake and a
  Claude Code tool call. Still open: Gemini CLI (the Google login is ineligible, no model turn),
  OpenAI-compatible against a real server (only a stub ran), MCP registration with Codex, Gemini
  and opencode (commands checked with `--help` only).

## Traps

- Integration tests use scripted fake agents (`tests/fake-agents`); no model is needed. They start
  real `uppd` processes on free ports, so a firewall or a sandbox without loopback breaks them.
- `/health` is open by design; every other route needs the token when one is configured.
