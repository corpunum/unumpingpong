#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Single agent loop (A) vs UnumPingPong tables (B, C) on the same small coding tasks.

Every arm uses the same OpenAI-compatible endpoint and model, sees the same task statements
and the same unit tests, and is scored afterwards by the same external referee
(`python3 -m unittest -q test_solution` in each task directory).

  A  one conversation for all tasks. The model writes files, may run shell commands
     (<run>cmd</run>) to check itself, and ends by saying DONE. Nobody checks it but itself.
  B  one UnumPingPong table per task (fresh small context per ball); the referee runs after
     every answer and its output goes back to the player. v0.2 defaults: result cards, an
     effort ladder (low -> medium -> high per rally), --parallel auto.
  C  B plus a persistent orchestrator (the same local model) that reads every card and
     answers it (accept / return with a correction): orchestrator-side vs player-side tokens.

Headline metrics: verified tasks per minute and per 100k tokens (a task counts only when
the external unit tests pass afterwards).

    python3 bench/bench.py --base-url http://127.0.0.1:8080/v1 --model NAME --set hard --arm a,b
    python3 bench/bench.py --render bench/results.json          # rewrite results.md only
Standard library only (plus the `upp` binary for B and C). Results merge into bench/results.json.
"""
import argparse, json, os, re, shutil, subprocess, sys, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
SETS = {"easy": "tasks", "hard": "tasks-hard"}

ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
ap.add_argument("--base-url", default="http://127.0.0.1:8080/v1")
ap.add_argument("--model")
ap.add_argument("--set", choices=sorted(SETS), default="easy")
ap.add_argument("--arm", default="a,b", help="comma list of a, b, c")
ap.add_argument("--upp", default=os.path.join(HERE, "..", "target", "release", "upp"))
ap.add_argument("--work", default="/tmp/upp-bench", help="scratch directory (recreated per arm)")
ap.add_argument("--max-tokens", type=int, default=6000)
ap.add_argument("--temperature", type=float, default=0.2)
ap.add_argument("--a-max-turns", type=int, default=0, help="default: max(20, 3 x tasks)")
ap.add_argument("--b-max-rallies", type=int, default=4)
ap.add_argument("--effort", default="low,medium,high", help="B/C effort ladder; '' sends none")
ap.add_argument("--parallel", default="auto")
ap.add_argument("--out", default=HERE)
ap.add_argument("--render", metavar="RESULTS.json", help="only rewrite results.md from a results file")
a = ap.parse_args()
UPP = os.path.abspath(a.upp)
TASKS_DIR = os.path.join(HERE, SETS[a.set])
TASKS = sorted(d for d in os.listdir(TASKS_DIR) if os.path.isdir(os.path.join(TASKS_DIR, d)))


def referee(root):
    """{task: bool}: run the unit tests of every task directory."""
    res = {}
    for t in TASKS:
        try:
            r = subprocess.run([sys.executable, "-m", "unittest", "-q", "test_solution"], cwd=os.path.join(root, t),
                               capture_output=True, text=True, timeout=120)
            res[t] = r.returncode == 0
        except subprocess.TimeoutExpired:
            res[t] = False
    return res


def fresh(root):
    shutil.rmtree(root, ignore_errors=True)
    for t in TASKS:
        shutil.copytree(os.path.join(TASKS_DIR, t), os.path.join(root, t))


def endpoint_state():
    try:
        root = a.base_url.rstrip("/").removesuffix("/v1")
        with urllib.request.urlopen(root + "/health", timeout=5) as r:
            h = json.load(r)
        return {k: h.get(k) for k in ("in_flight", "queued", "slots", "busy")}
    except Exception as e:  # noqa: BLE001 - informational only
        return {"error": str(e)}


def chat(messages):
    body = json.dumps({"model": a.model, "messages": messages, "max_tokens": a.max_tokens,
                       "temperature": a.temperature}).encode()
    req = urllib.request.Request(a.base_url.rstrip("/") + "/chat/completions", body, {"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=900) as r:
        d = json.load(r)
    u = d.get("usage", {})
    return d["choices"][0]["message"].get("content") or "", u.get("prompt_tokens", 0), u.get("completion_tokens", 0)


def headline(r, n):
    mins = r["wall_s"] / 60
    r["verified_per_min"] = round(r["points"] / mins, 3) if mins else None
    r["verified_per_100k_tokens"] = round(r["points"] / r["tokens"] * 1e5, 2) if r["tokens"] else None
    r["tasks"] = n
    return r


# ---------------------------------------------------------------- arm A: one agent loop
A_SYSTEM = """You are a coding agent working in a directory with several task folders.
Each folder has solution.py (a stub with the specification in its docstring) and test_solution.py.
Make every task's tests pass by rewriting solution.py. Tools, used by writing them in your reply:
  * write a file: a fenced block whose info string is file:<path>, e.g.
    ```file:duration/solution.py
    ...complete new content...
    ```
  * run a shell command (cwd = the directory holding all task folders): <run>cd duration && python3 -m unittest -q test_solution</run>
    You will get its output back in the next message.
When you are finished with ALL tasks, reply with a line that says exactly DONE."""


def arm_a(root):
    fresh(root)
    max_turns = a.a_max_turns or max(20, 3 * len(TASKS))
    parts = []
    for t in TASKS:
        parts.append(f"=== {t}/solution.py ===\n{open(os.path.join(root, t, 'solution.py')).read()}\n"
                     f"=== {t}/test_solution.py ===\n{open(os.path.join(root, t, 'test_solution.py')).read()}")
    msgs = [{"role": "system", "content": A_SYSTEM},
            {"role": "user", "content": "Tasks:\n\n" + "\n\n".join(parts)}]
    t0 = time.time(); pt = ct = 0; turns = 0; declared = False; runs = 0; log = []
    while turns < max_turns:
        text, p, c = chat(msgs); turns += 1; pt += p; ct += c
        msgs.append({"role": "assistant", "content": text})
        acted = False
        for m in re.finditer(r"```file:([^\n`]+)\n(.*?)```", text, re.S):
            path = os.path.normpath(os.path.join(root, m.group(1).strip()))
            if path.startswith(root + os.sep):
                os.makedirs(os.path.dirname(path), exist_ok=True)
                open(path, "w").write(m.group(2)); acted = True
        outs = []
        for m in re.finditer(r"<run>(.*?)</run>", text, re.S):
            runs += 1; acted = True
            try:
                r = subprocess.run(m.group(1), shell=True, cwd=root, capture_output=True, text=True, timeout=60)
                outs.append(f"$ {m.group(1).strip()}\n(exit {r.returncode})\n{(r.stdout + r.stderr)[-2500:]}")
            except subprocess.TimeoutExpired:
                outs.append(f"$ {m.group(1).strip()}\n(timeout)")
        log.append({"turn": turns, "wrote_files": acted and not outs, "runs": len(outs), "tokens_out": c})
        if re.search(r"^\s*DONE\s*$", text, re.M) and not outs:
            declared = True; break
        msgs.append({"role": "user", "content": "\n\n".join(outs) if outs else
                     "No file written and no command run. Continue, or reply DONE if every task is finished."})
    wall = time.time() - t0
    ok = referee(root)
    pts = sum(ok.values())
    return headline({"arm": "A single agent loop", "points": pts, "per_task": ok, "turns": turns, "max_turns": max_turns,
                     "shell_runs": runs, "wall_s": round(wall, 1), "prompt_tokens": pt, "completion_tokens": ct,
                     "tokens": pt + ct, "declared_done": declared,
                     "claimed_done_but_failed": declared and pts < len(TASKS),
                     "failed_when_claimed_done": (len(TASKS) - pts) if declared else None, "log": log}, len(TASKS))


# ---------------------------------------------------------------- arms B and C: UnumPingPong tables
def upp_json(*args):
    r = subprocess.run([UPP, *args], capture_output=True, text=True)
    return json.loads(r.stdout) if r.stdout.strip().startswith("{") else {}


def arm_tables(root, with_orchestrator):
    fresh(root)
    effort = [e for e in a.effort.split(",") if e]
    player = (f'{{ kind = "openai", base_url = "{a.base_url}", model = "{a.model}", '
              f'temperature = {a.temperature}, max_tokens = {a.max_tokens} }}')
    toml = ['[defaults]', f'max_parallel = "{a.parallel}"' if a.parallel == "auto" else f'max_parallel = {a.parallel}',
            f'lease = {{ wall_secs = 1800, max_rallies = {a.b_max_rallies} }}',
            'apply = { write_file = "solution.py" }', f'player = {player}',
            'referee = [{ kind = "command", name = "unittest", run = "python3 -m unittest -q test_solution", timeout_secs = 120 }]']
    if effort:
        toml.append("effort = [" + ", ".join(f'"{e}"' for e in effort) + "]")
    if with_orchestrator:
        toml += ['', '[orchestrator]',
                 f'player = {{ kind = "openai", base_url = "{a.base_url}", model = "{a.model}", temperature = {a.temperature}, '
                 f'max_tokens = 3000, reasoning_effort = "low" }}',
                 'instructions = "Accept when the referee passed. When it failed, return the ball with the one most '
                 'likely cause, read from the referee output, in at most 3 lines."']
    toml.append('')
    for t in TASKS:
        toml += ['[[table]]', f'name = "{t}"', f'goal = "test_solution.py passes for {t}"', f'cwd = "{root}/{t}"',
                 'ball = """', 'Implement the stub so every test passes. Keep the same function/class names.',
                 'Current solution.py:', '```python', '{{file:solution.py}}', '```', 'Tests (test_solution.py):', '```python',
                 '{{file:test_solution.py}}', '```', '"""', '']
    match = os.path.join(root, "match.toml"); open(match, "w").write("\n".join(toml))
    db = os.path.join(root, "bench.db")
    t0 = time.time()
    r = subprocess.run([UPP, "--db", db, "run", match, "--json"], capture_output=True, text=True, timeout=5400)
    wall = time.time() - t0
    ok = referee(root)
    rows = [json.loads(l) for l in subprocess.run([UPP, "--db", db, "export"], capture_output=True, text=True).stdout.splitlines() if l.strip()]
    summary = json.loads(r.stdout) if r.stdout.strip().startswith("{") else {"tables": []}
    cards = upp_json("--db", db, "cards", "--since", "0").get("cards", [])
    rally_cards = [c for c in cards if c.get("kind") == "rally"]
    per_table = {}
    for x in rows:
        p = per_table.setdefault(x["table"], {"rallies": 0, "completion_tokens": []})
        p["rallies"] += 1; p["completion_tokens"].append(x.get("tokens_out", 0))
    player_tokens = sum(x.get("tokens_in", 0) + x.get("tokens_out", 0) for x in rows)
    orch_tokens = sum(t.get("orchestrator_tokens", 0) for t in summary.get("tables", []))
    pts = sum(ok.values())
    return headline({
        "arm": "C tables + persistent orchestrator" if with_orchestrator else "B UnumPingPong tables",
        "points": pts, "per_task": ok, "rallies": len(rows), "faults": sum(1 for x in rows if x["verdict"] == "fault"),
        "wall_s": round(wall, 1), "effort": effort, "parallel": a.parallel,
        "parallel_used": re.search(r"--parallel auto -> (\d+)", r.stderr).group(1) if "--parallel auto" in r.stderr else a.parallel,
        "prompt_tokens": sum(x.get("tokens_in", 0) for x in rows), "completion_tokens": sum(x.get("tokens_out", 0) for x in rows),
        "player_tokens": player_tokens, "orchestrator_tokens": orch_tokens, "tokens": player_tokens + orch_tokens,
        "cards_from_block": sum(1 for c in rally_cards if c.get("source") == "block"),
        "rally_cards": len(rally_cards),
        # Claims the player wrote itself (its <card> block), not the extractor's guesses.
        "block_cards_claiming_done": sum(1 for c in rally_cards if c.get("source") == "block" and c.get("done")),
        "player_claimed_done_referee_failed": sum(1 for c in rally_cards if c.get("source") == "block"
                                                  and c.get("done") and not c.get("referee", {}).get("passed")),
        "rallies_at_max_tokens": sum(1 for x in rows if x.get("tokens_out", 0) >= a.max_tokens),
        "per_table": per_table, "upp_stderr_tail": r.stderr[-800:],
    }, len(TASKS))


# ---------------------------------------------------------------- rendering
def fmt_arm(r, key):
    return {
        "points": lambda: f"{r['points']}/{r['tasks']}",
        "vpm": lambda: f"{r['verified_per_min']:.2f}",
        "vpt": lambda: f"{r['verified_per_100k_tokens']:.1f}",
        "wall": lambda: f"{r['wall_s']:.0f} s",
        "tokens": lambda: f"{r['tokens']:,} ({r['prompt_tokens']:,} + {r['completion_tokens']:,})"
        + (f" + {r['orchestrator_tokens']:,} orch." if r.get("orchestrator_tokens") else ""),
        "calls": lambda: (f"{r['turns']} turns ({r['shell_runs']} self-checks)" if "turns" in r
                          else f"{r['rallies']} rallies" + (f" ({r['faults']} faults)" if r.get("faults") else "")),
        "false": lambda: ((f"{'yes' if r['claimed_done_but_failed'] else 'no'}: declared DONE with "
                           f"{r['failed_when_claimed_done']} of {r['tasks']} failing") if r.get("declared_done")
                          else ("never declared DONE" if "turns" in r
                                else (f"{r['player_claimed_done_referee_failed']} of {r['block_cards_claiming_done']} "
                                      "player cards claiming done (caught by the referee)")
                                if "block_cards_claiming_done" in r else "n/a (no cards in v0.1)")),
        "split": lambda: (f"player {r['player_tokens']:,} / orchestrator {r['orchestrator_tokens']:,}" if "player_tokens" in r else "n/a"),
    }[key]()


def render(res, out):
    L = ["# Benchmark: single agent loop vs UnumPingPong tables", "",
         f"Model `{res['model']}` at a local OpenAI-compatible endpoint (Halogen, 3 slots), temperature "
         f"{res['temperature']}, max_tokens {res.get('max_tokens', 6000)} per call. One run per arm. "
         "Headline metric: **verified tasks per minute and per 100k tokens** (a task counts only when its unit "
         "tests pass afterwards, whatever the arm claimed).", ""]
    rows = [("Verified tasks (unit tests afterwards)", "points"), ("Verified tasks per minute", "vpm"),
            ("Verified tasks per 100k tokens", "vpt"), ("Wall time", "wall"), ("Tokens (prompt + completion)", "tokens"),
            ("Model calls", "calls"), ("False \"done\"", "false"), ("Token split", "split")]
    for name, s in res.get("sets", {}).items():
        arms = [(k, s[k]) for k in ("A", "B_v01", "B", "C") if k in s]
        if not arms:
            continue
        L += [f"## {name.capitalize()} set: {len(s['tasks'])} tasks ({', '.join(s['tasks'])})", ""]
        label = lambda k, r: {"B_v01": "B v0.1 tables (no effort hint)", "B": "B v0.2 tables"}.get(k, r["arm"])
        L.append("| Metric | " + " | ".join(label(k, r) + (f" ({r['date']})" if r.get("date") else "") for k, r in arms) + " |")
        L.append("| --- |" + " --- |" * len(arms))
        for label, key in rows:
            L.append(f"| {label} | " + " | ".join(fmt_arm(r, key) for _, r in arms) + " |")
        L.append("")
        for k, r in arms:
            if k == "A":
                L.append(f"- A: reasoning effort = server default; {r['turns']} of max {r.get('max_turns', 20)} turns.")
            elif k == "B_v01":
                L.append("- B v0.1: no effort hint (server default), parallel 3, no cards.")
            else:
                extra = f"effort ladder {'/'.join(r['effort']) or 'none'}, parallel {r['parallel']} -> {r['parallel_used']}"
                L.append(f"- {k}: {extra}; result cards from the player's block: {r['cards_from_block']} of "
                         f"{r['rally_cards']}; rallies that hit max_tokens: {r.get('rallies_at_max_tokens', 'n/a')}."
                         + (f" {r['note']}" if r.get("note") else ""))
        failed = {k: [t for t, v in r["per_task"].items() if not v] for k, r in arms}
        L.append("- Tasks not verified: " + "; ".join(f"{k}: {', '.join(v) or 'none'}" for k, v in failed.items()) + ".")
        L.append("")
    L += res.get("notes_md", "").splitlines()
    open(os.path.join(out, "results.md"), "w").write("\n".join(L).rstrip() + "\n")
    print("\n".join(L))


if a.render:
    render(json.load(open(a.render)), a.out); sys.exit(0)
if not a.model:
    ap.error("--model is required")

path = os.path.join(a.out, "results.json")
res = json.load(open(path)) if os.path.exists(path) else {}
res.update({"model": a.model, "base_url": a.base_url, "temperature": a.temperature, "max_tokens": a.max_tokens})
s = res.setdefault("sets", {}).setdefault(a.set, {})
s["tasks"] = TASKS
for arm in [x.strip() for x in a.arm.split(",") if x.strip()]:
    st = endpoint_state()
    work = os.path.join(a.work, a.set, arm)
    r = arm_a(work) if arm == "a" else arm_tables(work, arm == "c")
    r["date"] = time.strftime("%Y-%m-%d"); r["endpoint_at_start"] = st
    s[arm.upper()] = r
    print(arm.upper(), json.dumps({k: v for k, v in r.items() if k not in ("log", "per_table", "upp_stderr_tail")}), flush=True)
    json.dump(res, open(path, "w"), indent=1)
render(res, a.out)
