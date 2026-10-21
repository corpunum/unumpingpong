# Open benchmark: real bug fixes from public repositories

12 real bug fixes from three public Python libraries ([more-itertools](https://github.com/more-itertools/more-itertools), [tomlkit](https://github.com/python-poetry/tomlkit), [toolz](https://github.com/pytoolz/toolz)); anyone can reproduce it: each task is a fresh repository made from the fix's parent commit (`git archive`, no history, so the fix cannot be looked up) plus the fix's own unit test (which fails there). The player gets the issue title and the failing test, not the file to change. A task counts only if the test passes afterwards and the test file is unchanged. Tasks: [tasks.json](tasks.json). One run per cell.

| Player | Arm | Verified | Wall | Verified / hour | Billable tokens | Cost | Rallies |
|---|---|---|---|---|---|---|---|
| haiku | A one agent session | 12/12 | 4.3 min | 167.66 | 305,072 | $0.051 | - |
| haiku | B UnumPingPong tables | 12/12 | 5.0 min | 143.43 | 320,923 | $0.048 | 12 |
| oc:nvidia/nemotron-3-ultra-550b-a55b | A one agent session | 10/12 | 41.5 min | 14.45 | 2,247,763 | - | - |
| oc:nvidia/nemotron-3-ultra-550b-a55b | B UnumPingPong tables | 12/12 | 44.3 min | 16.24 | 3,831,489 | - | 12 |
| haiku r2 | B UnumPingPong tables | 12/12 | 3.9 min | 183.85 | 231,663 | $0.048 | 12 |

Billable tokens = fresh input + output + 0.1 x cached input. "not recorded": the `openunum` player (local Qwen, and Luna inside the OpenUnum agent loop) did not report tokens in these runs.

Caveat: runs before 2026-10-20 18:50 UTC kept the referee script inside the agent's checkout, so tampering with it was not ruled out for them; later runs keep it outside. The opencode and Codex players ran in a rootless podman sandbox that can write only the task folder ([agent-sandbox.sh](agent-sandbox.sh)).

- haiku A: missed none
- haiku B: missed none
- oc:nvidia/nemotron-3-ultra-550b-a55b A: missed toolz-d2eba03, toolz-5a7e078
- oc:nvidia/nemotron-3-ultra-550b-a55b B: missed none
- haiku r2 B: missed none

## Verdict per player

Each player's tables (B) against its own single session (A); players run only as tables are compared with the named baseline. Tokens are billable tokens.

### Solves as much or more, with fewer tokens

- **haiku r2** (baseline: haiku A): 12/12 -> 12/12, 305,072 -> 231,663 tokens (-24%), 4.3 -> 3.9 min

### Solves more, at a higher token cost (better solutions)

- **oc:nvidia/nemotron-3-ultra-550b-a55b**: 10/12 -> 12/12, 2,247,763 -> 3,831,489 tokens (+70%), 41.5 -> 44.3 min

### No gain or worse (harness or model specific)

- **haiku**: 12/12 -> 12/12, 305,072 -> 320,923 tokens (+5%), 4.3 -> 5.0 min

