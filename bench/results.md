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

## Easy set with cloud players: v0.2.0 vs v0.2.1

| Metric | Haiku A v0.2.0 | Haiku B v0.2.0 | Haiku A v0.2.1 | Haiku B v0.2.1 | Luna low A v0.2.0 | Luna low B v0.2.0 | Luna low A v0.2.1 | Luna low B v0.2.1 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Verified tasks | 6/6 | 6/6 | 6/6 | 6/6 | 6/6 | 6/6 | 6/6 | 6/6 |
| Wall time | 36 s | 30 s | 33 s | 17 s | 33 s | 34 s | 29 s | 21 s |
| **Billable tokens** (fresh in + out + 0.1 x cached) | **35.8k** | **n/a** | **29.3k** | **18.4k** | **25.9k** | **n/a** | **22.5k** | **24.6k** |
| Fresh input / cached input / output | 20.4k / 72.8k / 8.0k | n/a | 14.4k / 75.9k / 7.3k | 9.1k / 0.0k / 9.3k | 11.8k / 105.1k / 3.6k | n/a | 14.0k / 51.2k / 3.4k | 20.2k / 2.0k / 4.2k |
| All tokens processed | 101.3k | 145.4k | 97.6k | 18.4k | 120.5k | 337.4k | 68.6k | 26.4k |
| Cost reported by the CLI | $0.0088 | $0.0081 | $0.0073 | $0.0065 | none | none | none | none |
| Verified per 100k billable tokens | 16.8 | n/a | 20.5 | 32.6 | 23.2 | n/a | 26.6 | 24.4 |
| Model calls | 11 turns | 6 rallies | 12 turns | 6 rallies | one session | 7 rallies | one session | 7 rallies |

## Hard set with cloud players: v0.2.0 vs v0.2.1

| Metric | Haiku A v0.2.0 | Haiku B v0.2.0 | Haiku A v0.2.1 | Haiku B v0.2.1 | Luna low A v0.2.0 | Luna low B v0.2.0 | Luna low A v0.2.1 | Luna low B v0.2.1 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Verified tasks | 8/8 | 8/8 | 8/8 | 8/8 | 8/8 | 8/8 | 7/8 | 8/8 |
| Wall time | 79 s | 63 s | 93 s | 34 s | 53 s | 47 s | 88 s | 23 s |
| **Billable tokens** (fresh in + out + 0.1 x cached) | **77.1k** | **n/a** | **86.0k** | **41.1k** | **57.8k** | **n/a** | **76.6k** | **33.2k** |
| Fresh input / cached input / output | 39.1k / 197.4k / 18.3k | n/a | 39.2k / 273.6k / 19.5k | 19.7k / 1.9k / 21.2k | 21.4k / 297.0k / 6.7k | n/a | 31.3k / 355.6k / 9.8k | 26.8k / 2.0k / 6.2k |
| All tokens processed | 254.8k | 407.2k | 332.2k | 42.8k | 325.0k | 436.0k | 396.7k | 35.0k |
| Cost reported by the CLI | $0.0189 | $0.0463 | $0.0203 | $0.0146 | none | none | none | none |
| Verified per 100k billable tokens | 10.4 | n/a | 9.3 | 19.5 | 13.8 | n/a | 9.1 | 24.1 |
| Model calls | 23 turns | 10 rallies | 29 turns | 9 rallies | one session | 9 rallies | one session | 9 rallies |

### Hard set: what each change does (B tables, one run each)

| Run | persistent | lean | tools | system prompt | UnumSearch MCP | verified | billable | fresh / cached / out | cost | rallies |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Haiku B0 | no | no (v0.2.0 flags) | default | CLI default | off | 8/8 | 121.0k | 73.7k / 183.1k / 29.0k | $0.0311 | 8 |
| Haiku B1 | yes | no (v0.2.0 flags) | default | CLI default | off | 8/8 | 129.7k | 79.5k / 177.8k / 32.4k | $0.0339 | 12 |
| Haiku B2 | yes | yes | default | CLI default | off | 8/8 | 92.2k | 61.0k / 62.5k / 25.0k | $0.0253 | 9 |
| Haiku B3 | yes | yes | none | CLI default | off | 8/8 | 62.1k | 30.0k / 26.0k / 29.5k | $0.0210 | 10 |
| Haiku B4 | yes | yes | none | short | off | 8/8 | 41.1k | 19.7k / 1.9k / 21.2k | $0.0146 | 9 |
| Haiku B5 | yes | yes | none | short | on | 8/8 | 41.3k | 19.4k / 8.7k / 21.1k | $0.0145 | 9 |
| Luna low B3 | yes | yes | none | CLI default | off | 8/8 | 150.7k | 108.5k / 235.4k / 18.7k | none | 14 |
| Luna low B | yes | yes | none | short | off | 8/8 | 33.2k | 26.8k / 2.0k / 6.2k | none | 9 |

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

## Cloud players (Claude Haiku, Luna low): token carry fixed in v0.2.1, 2026-10-19

Same task sets, same unit-test referee, real agent CLIs, no local endpoint: Claude Code 2.1.303
`--model haiku` (Haiku 5.5) and codex-cli 0.160.0 `-m gpt-5.6-luna -c model_reasoning_effort="low"`.
One run per arm. **A** = one CLI session over all tasks: it edits files, runs the tests itself and
ends with DONE or NOT DONE. **B** = one UnumPingPong table per task, parallel 3, effort ladder
low/medium/high, `apply = { write_file = "solution.py" }` (the player's code block is written for
it and the referee runs the tests).

**Token metric.** *Billable tokens* = fresh input (cache writes included) + output + cache reads
x 0.1 (Anthropic and OpenAI bill cache reads at about a tenth). "All tokens processed" counts cache
reads in full, which is what the v0.2.0 tables showed; v0.2.0's B runs did not record the cached
part apart, so their billable count is n/a (their cost, for Haiku, is real). Claude reports cost,
Codex does not.

**v0.2.0 problem.** Every rally was a fresh CLI invocation: the CLI's system prompt and 10-25k
tokens of tool/MCP schemas went out again on every call, the agent then used those tools inside
the rally (read the file it had just been sent, ran the tests the referee was about to run), and
the "tokens" column added cache reads to fresh input. B cost 1.3-2.8x A (Haiku hard $0.046 vs
$0.019).

**What changed for CLI players (v0.2.1)**

- *Persistent process*: one `claude --input-format stream-json` process or one `codex app-server`
  thread per table, each rally a new turn. Measured alone (Haiku hard, B0 -> B1) it saves no tokens:
  the API is stateless, so a resumed session and a live one send the same conversation, and the
  prompt cache was already warm. It saves process start-up time and keeps the session explicit.
- *Delta-only returns*: a returned ball carries the failing checks with the head and tail of their
  output (about 2 KB at most) and one line; never the brief again. Small here (few returns).
- *Lean context*: no MCP servers, settings, skills, memory or extra feature tools (Claude 15.3k ->
  3.6k tokens of context with `tools = []`; Codex 12.9k -> 6.5k). Note that v0.2.0's
  `-c mcp_servers={}` did not actually turn Codex's MCP servers off. Haiku hard B1 -> B2: 130k -> 92k
  billable.
- *Tools only as needed*: a table that writes the answer itself needs no player tools
  (`tools = []`): Haiku hard B2 -> B3: 92k -> 62k, and the agent stops re-running tests.
- *Short system prompt* (`system_prompt`, a 3-sentence player prompt in place of the CLI's agent
  prompt): Haiku hard B3 -> B4: 62k -> 41k. For Codex this is the decisive step: without it Luna
  still acted as a full agent inside each rally (code-mode tools, apply_patch, self-tests), hard B3
  = 151k billable, 2x its A; with it 33k.

**Result (A and B both with the lean flags; B as in the last ablation row).**

- Haiku: easy 18.4k vs 29.3k billable (0.63x), $0.0065 vs $0.0073; hard 41.1k vs 86.0k (0.48x),
  **$0.0146 vs $0.0203 (0.72x)**. Cost falls less than tokens because output (the full file per
  answer) dominates B's bill and is about the same as A's.
- Luna low: hard 33.2k vs 76.6k (0.43x), and B verified 8/8 where A verified 7/8 (A said
  NOT DONE honestly: `isoweek` week-53 boundary). **Easy: B used slightly more, 24.6k vs 22.5k
  (1.09x)**: six small tasks, each table pays its own uncached first call (B fresh input 20.2k,
  cached 2k), while A's one session re-reads a warm cache at a tenth of the price. On sets of
  small tasks a single Codex session stays a bit cheaper; tables win once tasks need several
  turns of context.
- Wall time: B was faster in all four pairs (17 vs 33 s, 21 vs 29 s, 34 vs 93 s, 23 vs 88 s).
- The referee still mattered: B's players claimed done in their cards and the referee returned
  one ball each on Luna easy (`semver`), Haiku hard (`glob`) and Luna hard (`csvline`), each fixed
  on the next rally in the same session.
- *UnumSearch on* (Haiku hard B5 = B4 + `search = "unumsearch"`, the UnumSearch MCP server as the
  player's only tool): 41.3k billable, $0.0145, 8/8, the same as B4 within noise. The tool
  schemas add about 2k tokens of context, mostly read from cache; these tasks hand the player the
  whole file, so there is nothing to search for. Search pays off where the brief does not carry
  the code (a repository task); chat players without tools get it through `context_search` /
  `{{search:...}}` instead.
- One run per arm and model calls are not deterministic (B1 needed 12 rallies where B0 needed 8);
  treat differences under ~15% as noise. The ablation ran on Haiku only (cheap); Codex has A, B and
  B without the short prompt.
- A is not comparable to local Qwen's A: Haiku/Luna are full agents with their own tool loops and
  run 10-20x faster than the local model, so wall columns reflect hardware and provider.

Reproduce:

    python3 bench/bench.py --player claude --label haiku --cli-model haiku --set easy|hard --arm a,b --parallel 3 --b-system-prompt 1
    python3 bench/bench.py --player codex --label luna-low --cli-model gpt-5.6-luna --cli-effort low \
        --effort low,medium,high --set easy|hard --arm a,b --parallel 3 --b-system-prompt 1
    # ablation: --arm b --arm-key B0 --b-persistent 0 --b-lean 0 --b-tools default   (B1..B4 likewise)
