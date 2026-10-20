# Real-repository benchmark

10 real bug fixes from OpenUnum (`corpunum/openunum`, a private repository; the harness works on any repository): each task is a fresh repository made from the fix's parent commit (`git archive`, no history, so the fix cannot be looked up) plus the fix's own unit test (which fails there). The player gets the issue title and the failing test, not the file to change. A task counts only if the test passes afterwards and the test file is unchanged. Tasks: [tasks.json](tasks.json). One run per cell.

| Player | Arm | Verified | Wall | Verified / hour | Billable tokens | Cost | Rallies |
|---|---|---|---|---|---|---|---|
| luna-low | A one agent session | 7/10 | 1.2 min | 347.36 | 165,465 | - | - |
| luna-low | B UnumPingPong tables | 10/10 | 2.9 min | 208.09 | 766,817 | - | 10 |
| luna-low | C tables, one shared session | 10/10 | 5.1 min | 117.09 | 1,202,867 | - | 10 |
| qwen | A one agent session | 7/10 | 25.0 min | 16.8 | not recorded | - | - |
| qwen | B UnumPingPong tables | 9/10 | 101.8 min | 5.31 | not recorded | - | 10 |
| haiku | A one agent session | 10/10 | 9.4 min | 64.03 | 418,803 | $0.109 | - |
| haiku | B UnumPingPong tables | 10/10 | 1.2 min | 502.17 | 252,350 | $0.052 | 10 |
| oc:nemotron-3-ultra-free | A one agent session | 10/10 | 21.6 min | 27.8 | 2,214,721 | - | - |
| oc:nemotron-3-ultra-free | B UnumPingPong tables | 7/10 | 103.9 min | 4.04 | 2,393,368 | - | 12 |
| supergemma4-q5 (ROG Ally) | A one agent session | 1/10 | 7.3 min | 8.17 | 58,314 | - | - |
| supergemma4-q5 (ROG Ally) | B UnumPingPong tables | 1/10 | 52.7 min | 1.14 | 286,120 | - | 28 |
| gemma-4-E2B (S22 phone) | A one agent session | 0/3 | 60.0 min | 0.0 | 16,531 | - | - |
| gemma-4-E2B (S22 phone) | B UnumPingPong tables | 0/3 | 75.0 min | 0.0 | not recorded | - | 3 |
| luna-low lean-prompt | B UnumPingPong tables | 10/10 | 2.1 min | 280.29 | 502,605 | - | 10 |
| luna (OpenUnum agent loop) | B UnumPingPong tables | 10/10 | 1.8 min | 342.43 | not recorded | - | 10 |
| oc:openai/gpt-oss-20b | A one agent session | 0/10 | 10.0 min | 0.0 | 103,563 | - | - |
| oc:openai/gpt-oss-20b | B UnumPingPong tables | 7/10 | 89.5 min | 4.69 | 3,899,150 | - | 17 |
| oc:nvidia/nemotron-3-ultra-550b-a55b | A one agent session | 1/10 | 9.7 min | 6.19 | 1,624,164 | - | - |
| oc:nvidia/nemotron-3-ultra-550b-a55b | B UnumPingPong tables | 10/10 | 36.2 min | 16.59 | 3,243,551 | - | 10 |

Billable tokens = fresh input + output + 0.1 x cached input. "not recorded": the `openunum` player (local Qwen, and Luna inside the OpenUnum agent loop) did not report tokens in these runs.

Caveat: runs before 2026-10-20 18:50 UTC kept the referee script inside the agent's checkout, so tampering with it was not ruled out for them; later runs keep it outside. The opencode and Codex players ran in a rootless podman sandbox that can write only the task folder ([agent-sandbox.sh](agent-sandbox.sh)).

- luna-low A: missed sessions-f25516b, scheduler-3b7a832, intent-6bd97cf
- luna-low B: missed none
- luna-low C: missed none
- qwen A: missed tools-4707051, intent-6bd97cf, core-d3bd2fe
- qwen B: missed intent-6bd97cf
- haiku A: missed none
- haiku B: missed none
- oc:nemotron-3-ultra-free A: missed none
- oc:nemotron-3-ultra-free B: missed sessions-f25516b, intent-6bd97cf, core-d3bd2fe
- supergemma4-q5 (ROG Ally) A: missed tool-map-7681fe8, models-9151b9e, council-5f832b6, memory-e6d2f31, sessions-f25516b, scheduler-3b7a832, tools-4707051, intent-6bd97cf, core-d3bd2fe
- supergemma4-q5 (ROG Ally) B: missed trading-metrics-401870f, models-9151b9e, council-5f832b6, memory-e6d2f31, sessions-f25516b, scheduler-3b7a832, tools-4707051, intent-6bd97cf, core-d3bd2fe
- gemma-4-E2B (S22 phone) A: missed trading-metrics-401870f, models-9151b9e, council-5f832b6
- gemma-4-E2B (S22 phone) B: missed trading-metrics-401870f, models-9151b9e, council-5f832b6
- luna-low lean-prompt B: missed none
- luna (OpenUnum agent loop) B: missed none
- oc:openai/gpt-oss-20b A: missed trading-metrics-401870f, tool-map-7681fe8, models-9151b9e, council-5f832b6, memory-e6d2f31, sessions-f25516b, scheduler-3b7a832, tools-4707051, intent-6bd97cf, core-d3bd2fe
- oc:openai/gpt-oss-20b B: missed council-5f832b6, tools-4707051, intent-6bd97cf
- oc:nvidia/nemotron-3-ultra-550b-a55b A: missed tool-map-7681fe8, models-9151b9e, council-5f832b6, memory-e6d2f31, sessions-f25516b, scheduler-3b7a832, tools-4707051, intent-6bd97cf, core-d3bd2fe
- oc:nvidia/nemotron-3-ultra-550b-a55b B: missed none
