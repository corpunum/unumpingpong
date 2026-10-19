# Benchmark: single agent loop vs UnumPingPong tables

Model `openai/qwen3.8-flash-next-uncensored-lean` at a local OpenAI-compatible endpoint, 6 small Python tasks (calc, duration, intervals, lru, semver, wrap), temperature 0.2, run on 2026-10-19. One run per arm.

| Metric | A: one agent loop | B: UnumPingPong tables |
| --- | --- | --- |
| Points (referee-verified) | 6/6 | 6/6 |
| Model calls / rallies | 19 turns (8 self-checks) | 6 rallies |
| Rallies per point | 3.2 | 1.0 |
| Wall time | 354 s | 580 s (cap 3 in parallel) |
| Tokens (prompt + completion) | 132,635 (118,114 + 14,521) | 28,813 (3,422 + 25,391) |
| Claimed done but referee failed | no (declared done: True; 0 of 6 tasks failing at that point) | n/a (a point needs the referee) |

## Notes and caveats

- One run per arm, temperature 0.2, on an endpoint shared with other traffic, so wall times are
  noisy. Treat the token ratio as solid (it is structural: arm A re-sends one growing conversation
  on every turn) and the wall-time comparison as indicative only.
- Both arms scored 6/6, so this set shows no quality difference. Arm A also never claimed "done"
  while a test failed; the "claimed done but referee failed" rate is therefore 0/1 here. The set is
  easy for this model; a harder set or a weaker model is where the referee is expected to matter.
- Arm B was slower in wall time even with 3 tables in parallel: its fresh-context balls each made
  the model think at length (25.4k completion tokens in total against 14.5k for A), and the
  endpoint serves requests with limited concurrency.
- Arm A had a real self-check tool (`<run>` shell commands) and used it 8 times. Without it, the
  gap in false "done" claims would likely be larger; that variant was not measured.
- Reproduce: `python3 bench/bench.py --base-url http://127.0.0.1:8080/v1 --model NAME`
  (needs `target/release/upp`), then `python3 bench/card.py` for the result card.
