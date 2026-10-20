# Benchmarks (synthetic task sets)

The real-repository benchmark (10 real bug fixes) is in [bench/repo/results.md](../bench/repo/results.md) and on the README. Below: the earlier synthetic sets (small Python tasks), kept for history.

## Synthetic task sets

[`bench/`](../bench/) solves the same Python tasks with the same local model as (A) one agent loop,
a single conversation where the model self-checks and says DONE, (B) UnumPingPong tables (a
fresh small context per ball, referee feedback, cards, an effort ladder, `--parallel auto`), and
(C) B plus a persistent orchestrator on the same model. Every arm is scored afterwards by the
same unit tests. The headline metric is **verified tasks per minute and per 100k tokens**.

| Easy set, 6 tasks | A: one agent loop | B v0.1 tables | B v0.2 tables |
| --- | --- | --- | --- |
| Verified | 6/6 | 6/6 | 6/6 |
| Verified per 100k tokens | 4.5 | 20.8 | **33.0** |
| Verified per minute | **1.02** | 0.62 | 0.53 |
| Tokens | 132,635 | 28,813 | 18,185 |
| Completion tokens | 14,521 | 25,391 | 14,337 |
| Wall | 354 s | 580 s | 675 s (endpoint shared with 2-3 other streams) |

Findings, stated plainly:

- **Tokens:** tables need far fewer tokens for the same verified result (fresh small contexts;
  A re-sends one growing conversation every turn). The v0.2 effort ladder (low first, higher
  only after a return) cut B's completion tokens by 44% against v0.1.
- **Wall time:** the single loop is still faster per minute on this endpoint. Its 3 slots decode
  ~1.15x one stream in total (48 -> 56 tok/s measured), so wall time is roughly total
  completion tokens divided by the endpoint's decode rate, and parallel tables do not multiply
  it. The v0.2 run also shared the endpoint with 2-3 other streams. `--parallel auto` now leaves
  busy slots to other traffic.
- **Hard set (8 tasks, partial):** stopped at the 40-minute model-time budget with B at 1/8
  verified and **no A run yet**, so there is no comparison. It already shows the referee earning
  its place: in its own result cards the player claimed `done` 6 times and the referee rejected
  5 of them. 4 of 12 rallies spent the whole 6k-token budget, mostly reasoning, and truncated the
  code.

### Cloud players (Claude Haiku, Luna low)

The same sets with real agent CLIs as players, no local endpoint: Claude Code (`--model haiku`)
and Codex (`-m gpt-5.6-luna`, reasoning effort low). A is one CLI session over all tasks (it
edits, runs the tests itself, says DONE); B is one table per task, parallel 3, effort ladder.
One run per arm. **Billable tokens** = fresh input + output + cache reads x 0.1 (what the
providers bill); Claude also reports dollars.

v0.2.0 lost on tokens: every rally was a fresh CLI invocation that re-sent the CLI's system
prompt and 10-25k tokens of tool and MCP schemas, the agent re-did inside each rally what the
table already does (read the file it was sent, run the tests), and the count mixed cache reads
with fresh input: B used 1.3-2.8x A's tokens (Haiku hard $0.046 vs $0.019). v0.2.1 fixed the
carry (one process per table, delta-only returns, lean CLI context, `tools = []` and a short
`system_prompt` for a player whose answer the table writes):

| Set | Player | A one session: verified, wall, billable, cost | B tables: verified, wall, billable, cost | B / A billable |
| --- | --- | --- | --- | --- |
| Easy (6) | Haiku | 6/6, 33 s, 29.3k, $0.0073 | 6/6, 17 s, 18.4k, $0.0065 | 0.63x |
| Easy (6) | Luna low | 6/6, 29 s, 22.5k | 6/6, 21 s, 24.6k | **1.09x** |
| Hard (8) | Haiku | 8/8, 93 s, 86.0k, $0.0203 | 8/8, 34 s, 41.1k, $0.0146 | 0.48x |
| Hard (8) | Luna low | 7/8, 88 s, 76.6k | 8/8, 23 s, 33.2k | 0.43x |

- **What each change bought** (Haiku hard, B billable): v0.2.0 config 121k -> persistent process
  130k (no saving: the API re-sends the conversation either way; it saves start-up time) -> lean
  context 92k -> no player tools 62k -> short system prompt 41k. For Codex the short prompt is the
  decisive step: without it Luna still worked as a full agent inside each rally (151k, 2x A).
- **Where tables still cost more:** Luna on the easy set (1.09x). Six small tasks, each table pays
  its own first, uncached call, while A's single session re-reads a warm cache at a tenth of the
  price. Haiku's dollar saving (0.72x hard, 0.88x easy) is smaller than its token saving because
  output (a full file per answer) dominates B's bill.
- Tables were faster in all four pairs, verified 8/8 where Luna's single session verified 7/8
  (it said NOT DONE honestly), and the referee returned three balls that the player had claimed
  done, each fixed on the next rally in the same session.
- Cloud A is not comparable to the Qwen A (a full agent with its own tool loop on much faster
  hardware); read these as a second reference, not a speed-up claim.

Full tables, the wall-time investigation, caveats and the reproduce command are in
[bench/results.md](../bench/results.md).

