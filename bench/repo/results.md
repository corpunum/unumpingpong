# Real-repository benchmark

10 real bug fixes from OpenUnum (`corpunum/openunum`, a private repository; the harness works on any repository): each task is a fresh repository made from the fix's parent commit (`git archive`, no history, so the fix cannot be looked up) plus the fix's own unit test (which fails there). The player gets the issue title and the failing test, not the file to change. A task counts only if the test passes afterwards and the test file is unchanged. Tasks: [tasks.json](tasks.json). One run per cell.

| Player | Arm | Verified | Wall | Verified / hour | Billable tokens | Cost | Rallies |
|---|---|---|---|---|---|---|---|
| qwen | A one agent session | 7/10 | 25.0 min | 16.8 | not recorded | - | - |
| qwen | B UnumPingPong tables | 9/10 | 101.8 min | 5.31 | not recorded | - | 10 |
| haiku | A one agent session | 10/10 | 9.4 min | 64.03 | 418,803 | $0.109 | - |
| haiku | B UnumPingPong tables | 10/10 | 1.2 min | 502.17 | 252,350 | $0.052 | 10 |
| luna (OpenUnum agent loop) | B UnumPingPong tables | 10/10 | 1.8 min | 342.43 | not recorded | - | 10 |
| luna (OpenUnum agent loop) r2 | B UnumPingPong tables | 9/10 | 2.2 min | 246.41 | 1,630,638 | - | 12 |
| haiku r2 | A one agent session | 10/10 | 3.1 min | 196.29 | 405,719 | $0.079 | - |
| haiku r2 | B UnumPingPong tables | 10/10 | 0.9 min | 633.61 | 249,077 | $0.050 | 10 |
| luna (OpenUnum agent loop) r3 cache-key | B UnumPingPong tables | 10/10 | 1.6 min | 373.92 | 737,775 | - | 10 |
| codex cli (gpt-6-luna low) | A one agent session | 8/10 | 2.1 min | 224.2 | 215,372 | - | - |
| codex cli (gpt-6-luna low) | B UnumPingPong tables | 10/10 | 0.9 min | 633.6 | 322,173 | - | 10 |
| nanocode (gpt-6-luna low) | A one agent session | 10/10 | 2.2 min | 267.35 | 1,841,152 | - | - |
| nanocode (gpt-6-luna low) | B UnumPingPong tables | 10/10 | 1.3 min | 456.01 | 1,715,708 | - | 11 |
| oc:nvidia/nemotron-3-ultra-550b-a55b | A one agent session | 10/10 | 19.1 min | 31.37 | 2,089,092 | - | - |
| oc:nvidia/nemotron-3-ultra-550b-a55b | B UnumPingPong tables | 10/10 | 29.0 min | 20.71 | 1,496,904 | - | 11 |
| oc:openai/gpt-oss-20b | A one agent session | 1/10 | 3.7 min | 16.19 | 172,616 | - | - |
| oc:openai/gpt-oss-20b | B UnumPingPong tables | 6/10 | 140.8 min | 2.56 | 3,095,466 | - | 18 |
| oc:nemotron-3-ultra-free | A one agent session | 10/10 | 6.8 min | 88.35 | 1,423,536 | - | - |
| oc:nemotron-3-ultra-free | B UnumPingPong tables | 10/10 | 18.1 min | 33.22 | 1,430,334 | - | 10 |

Billable tokens = fresh input + output + 0.1 x cached input. "not recorded": the `openunum` player (local Qwen, and Luna inside the OpenUnum agent loop) did not report tokens in these runs.

Caveat: runs before 2026-10-20 18:50 UTC kept the referee script inside the agent's checkout, so tampering with it was not ruled out for them; later runs keep it outside. The opencode and Codex players ran in a rootless podman sandbox that can write only the task folder ([agent-sandbox.sh](agent-sandbox.sh)).

- qwen A: missed tools-4707051, intent-6bd97cf, core-d3bd2fe
- qwen B: missed intent-6bd97cf
- haiku A: missed none
- haiku B: missed none
- luna (OpenUnum agent loop) B: missed none
- luna (OpenUnum agent loop) r2 B: missed intent-6bd97cf
- haiku r2 A: missed none
- haiku r2 B: missed none
- luna (OpenUnum agent loop) r3 cache-key B: missed none
- codex cli (gpt-6-luna low) A: missed scheduler-3b7a832, intent-6bd97cf
- codex cli (gpt-6-luna low) B: missed none
- nanocode (gpt-6-luna low) A: missed none
- nanocode (gpt-6-luna low) B: missed none
- oc:nvidia/nemotron-3-ultra-550b-a55b A: missed none
- oc:nvidia/nemotron-3-ultra-550b-a55b B: missed none
- oc:openai/gpt-oss-20b A: missed tool-map-7681fe8, models-9151b9e, council-5f832b6, memory-e6d2f31, sessions-f25516b, scheduler-3b7a832, tools-4707051, intent-6bd97cf, core-d3bd2fe
- oc:openai/gpt-oss-20b B: missed tool-map-7681fe8, tools-4707051, intent-6bd97cf, core-d3bd2fe
- oc:nemotron-3-ultra-free A: missed none
- oc:nemotron-3-ultra-free B: missed none

## Verdict per player

Each player's tables (B) against its own single session (A); players run only as tables are compared with the named baseline. Tokens are billable tokens.

### Solves as much or more, with fewer tokens

- **haiku**: 10/10 -> 10/10, 418,803 -> 252,350 tokens (-40%), 9.4 -> 1.2 min
- **haiku r2**: 10/10 -> 10/10, 405,719 -> 249,077 tokens (-39%), 3.1 -> 0.9 min
- **nanocode (gpt-6-luna low)**: 10/10 -> 10/10, 1,841,152 -> 1,715,708 tokens (-7%), 2.2 -> 1.3 min
- **oc:nvidia/nemotron-3-ultra-550b-a55b**: 10/10 -> 10/10, 2,089,092 -> 1,496,904 tokens (-28%), 19.1 -> 29.0 min

### Solves more, at a higher token cost (better solutions)

- **qwen**: 7/10 -> 9/10, tokens not recorded, 25.0 -> 101.8 min
- **luna (OpenUnum agent loop)** (baseline: codex cli (gpt-6-luna low) A): 8/10 -> 10/10, tokens not recorded, 2.1 -> 1.8 min
- **luna (OpenUnum agent loop) r2** (baseline: codex cli (gpt-6-luna low) A): 8/10 -> 9/10, 215,372 -> 1,630,638 tokens (+657%), 2.1 -> 2.2 min
- **luna (OpenUnum agent loop) r3 cache-key** (baseline: codex cli (gpt-6-luna low) A): 8/10 -> 10/10, 215,372 -> 737,775 tokens (+243%), 2.1 -> 1.6 min
- **codex cli (gpt-6-luna low)**: 8/10 -> 10/10, 215,372 -> 322,173 tokens (+50%), 2.1 -> 0.9 min
- **oc:openai/gpt-oss-20b**: 1/10 -> 6/10, 172,616 -> 3,095,466 tokens (+1693%), 3.7 -> 140.8 min

### No gain or worse (harness or model specific)

- **oc:nemotron-3-ultra-free**: 10/10 -> 10/10, 1,423,536 -> 1,430,334 tokens (+0%), 6.8 -> 18.1 min. already 10/10 alone; tables add rallies and time but no solved task

## Superseded runs

Kept for the record, left out of the verdicts above:

- luna-low: A 7/10, B 10/10, C 10/10. ran in the agent sandbox while node_modules was read-only: the agent could not run vitest (EROFS), so arm A was handicapped; fixed 10-21 (overlay mount)
- oc:nemotron-3-ultra-free: A 10/10, B 7/10. ran in the agent sandbox while node_modules was read-only: the agent could not run vitest (EROFS), so arm A was handicapped; fixed 10-21 (overlay mount)
- supergemma4-q5 (ROG Ally): A 1/10, B 1/10. ran in the agent sandbox while node_modules was read-only: the agent could not run vitest (EROFS), so arm A was handicapped; fixed 10-21 (overlay mount)
- gemma-4-E2B (S22 phone): A 0/3, B 0/3. ran in the agent sandbox while node_modules was read-only: the agent could not run vitest (EROFS), so arm A was handicapped; fixed 10-21 (overlay mount)
- luna-low lean-prompt: B 10/10. ran in the agent sandbox while node_modules was read-only: the agent could not run vitest (EROFS), so arm A was handicapped; fixed 10-21 (overlay mount)
- oc:openai/gpt-oss-20b: A 0/10, B 7/10. ran in the agent sandbox while node_modules was read-only: the agent could not run vitest (EROFS), so arm A was handicapped; fixed 10-21 (overlay mount)
- oc:nvidia/nemotron-3-ultra-550b-a55b: A 1/10, B 10/10. ran in the agent sandbox while node_modules was read-only: the agent could not run vitest (EROFS), so arm A was handicapped; fixed 10-21 (overlay mount)
- oc:nvidia/nemotron-3-ultra-550b-a55b r2: A 1/10, B 10/10. ran in the agent sandbox while node_modules was read-only: the agent could not run vitest (EROFS), so arm A was handicapped; fixed 10-21 (overlay mount)
- nanocode (gpt-6-luna low): A 5/10, B 9/10. ran in the agent sandbox while node_modules was read-only: the agent could not run vitest (EROFS), so arm A was handicapped; fixed 10-21 (overlay mount)
- codex cli (gpt-6-luna low): A 0/10, B 10/10. ran in the agent sandbox while node_modules was read-only: the agent could not run vitest (EROFS), so arm A was handicapped; fixed 10-21 (overlay mount)

