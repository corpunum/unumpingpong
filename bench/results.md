# Benchmark: single agent loop vs UnumPingPong tables

Model `openai/qwen3.8-flash-next-uncensored-lean` at a local OpenAI-compatible endpoint (Halogen, 3 slots), temperature 0.2, max_tokens 6000 per call. One run per arm. Headline metric: **verified tasks per minute and per 100k tokens** (a task counts only when its unit tests pass afterwards, whatever the arm claimed).

## Easy set: 6 tasks (calc, duration, intervals, lru, semver, wrap)

| Metric | A single agent loop (v0.1 run, 2026-10-19) | B v0.1 tables (no effort hint) (v0.1 run, 2026-10-19) | B v0.2 tables (2026-10-19) |
| --- | --- | --- | --- |
| Verified tasks (unit tests afterwards) | 6/6 | 6/6 | 6/6 |
| Verified tasks per minute | 1.02 | 0.62 | 0.53 |
| Verified tasks per 100k tokens | 4.5 | 20.8 | 33.0 |
| Wall time | 354 s | 580 s | 675 s |
| Tokens (prompt + completion) | 132,635 (118,114 + 14,521) | 28,813 (3,422 + 25,391) | 18,185 (3,848 + 14,337) |
| Model calls | 19 turns (8 self-checks) | 6 rallies | 6 rallies |
| False "done" | no: declared DONE with 0 of 6 failing | n/a (no cards in v0.1) | 0 of 6 player cards claiming done (caught by the referee) |
| Token split | n/a | n/a | player 18,185 / orchestrator 0 |

- A: reasoning effort = server default; 19 of max 20 turns.
- B v0.1: no effort hint (server default), parallel 3, no cards.
- B: effort ladder low/medium/high, parallel auto -> 3; result cards from the player's block: 6 of 6; rallies that hit max_tokens: 0.
- Tasks not verified: A: none; B_v01: none; B: none.

## Hard set: 8 tasks (bankround, csvline, glob, isoweek, jsonptr, moneysplit, roman, topo)

| Metric | B v0.2 tables (2026-10-19) |
| --- | --- |
| Verified tasks (unit tests afterwards) | 1/8 |
| Verified tasks per minute | 0.04 |
| Verified tasks per 100k tokens | 1.8 |
| Wall time | 1476 s |
| Tokens (prompt + completion) | 55,992 (17,725 + 38,267) |
| Model calls | 12 rallies (3 faults) |
| False "done" | 5 of 6 player cards claiming done (caught by the referee) |
| Token split | player 55,992 / orchestrator 0 |

- B: effort ladder low/medium/high, parallel auto -> 3; result cards from the player's block: 6 of 9; rallies that hit max_tokens: 4. Capped: the remaining tables were stopped at 25 min (model-time budget on a shared endpoint); 3 faults are those cancelled calls; 4 tables never started.
- Tasks not verified: B: bankround, csvline, isoweek, jsonptr, moneysplit, roman, topo.

## Why B was slower in v0.1 (580 s vs 354 s), and what changed

Measured on the endpoint (one model, Halogen 0.17.5, 3 admission slots), 2026-10-19:

| Probe | Result |
| --- | --- |
| 1 stream, `reasoning_effort = none`, 400-token list | 48.4 tok/s |
| 3 concurrent streams, same request | 55.7 tok/s in total (18.6 each): **1.15x**, not 3x |
| v0.1 B, completion tokens per ball (6 balls, server default effort `xhigh`) | 1.5k-5.8k; 4 of 6 balls ran to 5.3-5.8k of the 6k budget |
| v0.1 A, completion tokens | 14.5k in total: one long think (5.6k) in turn 1 covered all six tasks |
| Per-rally engine overhead (referee run, store writes, scheduling) | under 1 s per rally, against 100-500 s model calls |

So the gap was **not** the parallel cap and not per-rally overhead. The endpoint's three slots
are admission, not throughput: three streams decode about as fast as one in total. Wall time is
therefore about *total completion tokens / the endpoint's aggregate decode rate*, and v0.1's
fresh-context balls each did a long `xhigh` think (25.4k completion tokens against A's 14.5k).

Changes in v0.2:

- **Effort hints per rally** (`effort = ["low", "medium", "high"]`): a cheap first try, more
  thinking only after the referee returned the ball. On the easy set this cut B's completion
  tokens from 25.4k to 14.3k (-44%) at the same 6/6.
- **`--parallel auto`** probes the slot count (3 here), and now also leaves the requests the
  endpoint is already serving to that traffic (it did not in the run below: B took all 3 slots
  while the owner's own traffic was running, which queued their requests).
- No change can make three streams faster than the endpoint's aggregate decode rate; on this
  endpoint parallel tables mainly help when players are slow tools (CLIs, tests), not decode.

## Caveats (read before quoting numbers)

- **Shared, loaded endpoint.** The v0.2 runs (easy B, hard B) ran while 2-3 other streams were
  in flight on the same endpoint (`in_flight` 2-3, sometimes 3 queued), so their wall times are
  much longer than the v0.1 runs (which had a quieter endpoint) and are **not comparable** to
  them. Token counts and verified tasks per token are comparable; wall times only within a run.
- **Model-time budget.** The whole v0.2 benchmark was capped at about 40 minutes of model time
  on a shared endpoint. Easy A was not re-run (the v0.1 measurement is shown). Hard B was stopped
  after 25 minutes with 1 of 8 verified (4 tables never started), and **hard A was not run**, so
  there is no A vs B comparison on the hard set yet. Arm C (tables + a persistent orchestrator
  on the same model) is implemented and tested with scripted players, but was not run live for
  the same reason.
- **What the hard set already shows.** The referee matters there: in the player's own result
  cards it claimed `done: yes` 6 times and the referee rejected 5 of those (syntax errors from
  answers cut off at the token budget, a carry bug in banker's rounding, glob set edge cases, an
  ISO-week boundary). In 4 of 12 rallies the model used the entire 6,000-token budget, mostly on
  reasoning, even at `low` effort on rally 1, so the code was truncated: this hard set needs a
  larger `max_tokens` (or `effort = "none"` first) on this model.
- One run per arm, temperature 0.2. Treat everything as indicative.
- Reproduce: `cargo build --release`, then
  `python3 bench/bench.py --base-url URL --model NAME --set easy|hard --arm a,b,c`
  (results merge into bench/results.json), and `python3 bench/card.py` for the result card.
