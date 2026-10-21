# Open benchmark: real bug fixes from public repositories

Anyone can run this one. Each task is a **real fix commit** from a public Python library whose
own regression test fails on the parent commit and passes on the fix:

| Library | Tasks |
|---|---|
| [more-itertools](https://github.com/more-itertools/more-itertools) | 5 |
| [tomlkit](https://github.com/python-poetry/tomlkit) | 5 |
| [toolz](https://github.com/pytoolz/toolz) | 2 |

[tasks.json](tasks.json) only points at public commits (repository, parent, fix, test file and
test command); nothing is copied. The agent gets the issue line and the failing test, never the
file to change, and works on a fresh `git archive` of the parent (no history, so the fix cannot
be looked up). A task counts only if its test passes afterwards **and** the test file is unchanged.

## Results so far

One agent session doing all 12 (A) against UnumPingPong tables (B). Full table, per-task misses
and the verdict per player: [results.md](results.md).

| Player | A: one session | B: tables | Kind |
|---|---|---|---|
| Claude Haiku (Claude Code), run 1 | 12/12, 305k tokens | 12/12, 321k tokens | no gain |
| Claude Haiku, run 2 | | 12/12, 232k tokens | fewer tokens |
| Nemotron 3 Ultra, free on NVIDIA NIM (opencode) | 10/12, 2.2M tokens | **12/12**, 3.8M tokens | solves more, costs more |

What it says: these bugs are easier than the private OpenUnum set. A strong model (Haiku) solves
all 12 alone, so tables only change tokens and time, and two runs land on either side of the
single session (run-to-run noise). The open model missed both toolz tasks alone and solved them
at tables, where the referee's exact test failure went back to it.

**Contamination caveat:** these libraries are popular, so a model may have seen some fixes in
training. The referee still demands the real test to pass on the real code; compare players
against each other on the same set rather than reading absolute scores as skill.

## Every rally is published

[runs/](runs/) holds one JSONL file per table run (from `upp export`): every ball, every answer,
the referee's output and the tokens. Audit a run without re-running it. (Records start with
Haiku run 2; earlier runs kept only the totals.)

## Run it yourself

```bash
git clone https://github.com/corpunum/unumpingpong && cd unumpingpong
cargo build --release                                  # the upp binary
python3 bench/open/verify.py                           # every task: fails before, passes with the real fix
python3 bench/repo/repo_bench.py --tasks bench/open/tasks.json --player haiku --arm a,b
```

Players: `haiku` (Claude Code), `luna-low` (Codex CLI, `--model` to pick one), `opencode`
(`--model provider/model`, e.g. free NVIDIA NIM models), `nanocode`, `qwen` / `luna-openunum`
(OpenUnum). Needs git, Python 3 with pytest, and the player's CLI. Results land in
[results.json](results.json) / [results.md](results.md) next to the task file.

## Add tasks

[mine.py](mine.py) finds candidates in any Python repository: non-merge commits that change a
little source and a test, where the test fails on the parent (a real failure, not an import
error, so new features are skipped) and passes on the fix:

```bash
git clone https://github.com/OWNER/REPO /tmp/REPO
python3 bench/open/mine.py --repo /tmp/REPO --url https://github.com/OWNER/REPO --tests tests/ > candidates.jsonl
```

Pick bug fixes from the candidates, add them to `tasks.json`, and run `verify.py`. Pull requests
with new verified tasks, other languages, or results from other players are very welcome.
