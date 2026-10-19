#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Single agent loop (A) vs UnumPingPong tables (B) on the same small coding tasks.

Both arms use the same OpenAI-compatible endpoint and model, see the same task statement and
the same unit tests, and are scored by the same external referee afterwards
(`python3 -m unittest -q test_solution` in each task directory).

  A  one conversation for all tasks. The model writes files, may run shell commands
     (<run>cmd</run>) to check itself, and ends by saying DONE. Nobody checks it but itself.
  B  one UnumPingPong table per task (fresh small context per ball), the referee runs after
     every answer and its output goes back to the player; parallel cap 3, max 4 rallies.

    python3 bench/bench.py --base-url http://127.0.0.1:8080/v1 --model NAME [--arm a|b|both]
Standard library only (plus the `upp` binary for arm B). Writes bench/results.json and results.md.
"""
import argparse, json, os, re, shutil, subprocess, sys, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
TASKS = sorted(d for d in os.listdir(os.path.join(HERE, "tasks")) if os.path.isdir(os.path.join(HERE, "tasks", d)))

ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
ap.add_argument("--base-url", default="http://127.0.0.1:8080/v1")
ap.add_argument("--model", required=True)
ap.add_argument("--arm", choices=["a", "b", "both"], default="both")
ap.add_argument("--upp", default=os.path.join(HERE, "..", "target", "release", "upp"))
ap.add_argument("--work", default="/tmp/upp-bench", help="scratch directory (recreated)")
ap.add_argument("--max-tokens", type=int, default=6000)
ap.add_argument("--temperature", type=float, default=0.2)
ap.add_argument("--a-max-turns", type=int, default=20)
ap.add_argument("--b-max-rallies", type=int, default=4)
ap.add_argument("--parallel", type=int, default=3)
ap.add_argument("--out", default=HERE)
ap.add_argument("--render", metavar="RAW.json", help="only turn a results json into results.json/results.md")
a = ap.parse_args()
UPP = os.path.abspath(a.upp)


def referee(root):
    """Return {task: bool} by running the unit tests of every task directory."""
    res = {}
    for t in TASKS:
        r = subprocess.run([sys.executable, "-m", "unittest", "-q", "test_solution"], cwd=os.path.join(root, t),
                           capture_output=True, text=True, timeout=60)
        res[t] = r.returncode == 0
    return res


def fresh(root):
    shutil.rmtree(root, ignore_errors=True)
    for t in TASKS:
        shutil.copytree(os.path.join(HERE, "tasks", t), os.path.join(root, t))


def chat(messages):
    body = json.dumps({"model": a.model, "messages": messages, "max_tokens": a.max_tokens,
                       "temperature": a.temperature}).encode()
    req = urllib.request.Request(a.base_url.rstrip("/") + "/chat/completions", body, {"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=900) as r:
        d = json.load(r)
    u = d.get("usage", {})
    return d["choices"][0]["message"].get("content") or "", u.get("prompt_tokens", 0), u.get("completion_tokens", 0)


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
    parts = []
    for t in TASKS:
        parts.append(f"=== {t}/solution.py ===\n{open(os.path.join(root, t, 'solution.py')).read()}\n"
                     f"=== {t}/test_solution.py ===\n{open(os.path.join(root, t, 'test_solution.py')).read()}")
    msgs = [{"role": "system", "content": A_SYSTEM},
            {"role": "user", "content": "Tasks:\n\n" + "\n\n".join(parts)}]
    t0 = time.time(); pt = ct = 0; turns = 0; declared = False; runs = 0; log = []
    while turns < a.a_max_turns:
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
    return {"arm": "A single agent loop", "points": sum(ok.values()), "tasks": len(TASKS), "per_task": ok,
            "turns": turns, "shell_runs": runs, "wall_s": round(wall, 1), "prompt_tokens": pt, "completion_tokens": ct,
            "tokens": pt + ct, "declared_done": declared,
            "claimed_done_but_failed": declared and sum(ok.values()) < len(TASKS),
            "failed_when_claimed_done": (len(TASKS) - sum(ok.values())) if declared else None, "log": log}


# ---------------------------------------------------------------- arm B: UnumPingPong tables
def arm_b(root):
    fresh(root)
    toml = ['[defaults]', f'max_parallel = {a.parallel}',
            f'lease = {{ wall_secs = 900, max_rallies = {a.b_max_rallies} }}',
            'apply = { write_file = "solution.py" }',
            f'player = {{ kind = "openai", base_url = "{a.base_url}", model = "{a.model}", '
            f'temperature = {a.temperature}, max_tokens = {a.max_tokens} }}',
            'referee = [{ kind = "command", name = "unittest", run = "python3 -m unittest -q test_solution", timeout_secs = 60 }]', '']
    for t in TASKS:
        toml += ['[[table]]', f'name = "{t}"', f'goal = "test_solution.py passes for {t}"', f'cwd = "{root}/{t}"',
                 'ball = """', 'Implement the stub so every test passes. Keep the same function/class names.',
                 'Current solution.py:', '```python', '{{file:solution.py}}', '```', 'Tests (test_solution.py):', '```python',
                 '{{file:test_solution.py}}', '```', '"""', '']
    match = os.path.join(root, "match.toml"); open(match, "w").write("\n".join(toml))
    db = os.path.join(root, "bench.db")
    t0 = time.time()
    r = subprocess.run([UPP, "--db", db, "run", match, "--json"], capture_output=True, text=True, timeout=3600)
    wall = time.time() - t0
    ok = referee(root)
    rows = [json.loads(l) for l in subprocess.run([UPP, "--db", db, "export"], capture_output=True, text=True).stdout.splitlines() if l.strip()]
    return {"arm": "B UnumPingPong tables", "points": sum(ok.values()), "tasks": len(TASKS), "per_task": ok,
            "rallies": len(rows), "wall_s": round(wall, 1),
            "prompt_tokens": sum(x.get("tokens_in", 0) for x in rows), "completion_tokens": sum(x.get("tokens_out", 0) for x in rows),
            "tokens": sum(x.get("tokens_in", 0) + x.get("tokens_out", 0) for x in rows),
            "rallies_per_table": {x["table"] if "table" in x else str(x.get("table_id")): 0 for x in rows} and None, "rows_sample": rows[:1], "upp_stdout_tail": r.stdout[-1500:],
            "upp_stderr_tail": r.stderr[-500:]}


def render(res, out):
    A, B = res.get("A"), res.get("B")
    json.dump(res, open(os.path.join(out, "results.json"), "w"), indent=1)
    n = len(res["tasks"])
    L = [f"# Benchmark: single agent loop vs UnumPingPong tables", "",
         f"Model `{res['model']}` at a local OpenAI-compatible endpoint, {n} small Python tasks "
         f"({', '.join(res['tasks'])}), temperature {res['temperature']}, run on {res['date']}. One run per arm.", "",
         "| Metric | A: one agent loop | B: UnumPingPong tables |", "| --- | --- | --- |"]
    if A and B:
        L += [f"| Points (referee-verified) | {A['points']}/{n} | {B['points']}/{n} |",
              f"| Model calls / rallies | {A['turns']} turns ({A['shell_runs']} self-checks) | {B['rallies']} rallies |",
              f"| Rallies per point | {A['turns'] / max(A['points'], 1):.1f} | {B['rallies'] / max(B['points'], 1):.1f} |",
              f"| Wall time | {A['wall_s']:.0f} s | {B['wall_s']:.0f} s (cap 3 in parallel) |",
              f"| Tokens (prompt + completion) | {A['tokens']:,} ({A['prompt_tokens']:,} + {A['completion_tokens']:,}) | {B['tokens']:,} ({B['prompt_tokens']:,} + {B['completion_tokens']:,}) |",
              f"| Claimed done but referee failed | {'yes' if A['claimed_done_but_failed'] else 'no'} (declared done: {A['declared_done']}; {A['failed_when_claimed_done']} of {n} tasks failing at that point) | n/a (a point needs the referee) |"]
    open(os.path.join(out, "results.md"), "w").write("\n".join(L) + "\n")
    print("\n".join(L))


if a.render:
    render(json.load(open(a.render)), a.out); sys.exit(0)

results = {"model": a.model, "base_url": a.base_url, "tasks": TASKS, "date": time.strftime("%Y-%m-%d"), "temperature": a.temperature}
if a.arm in ("a", "both"):
    results["A"] = arm_a(os.path.join(a.work, "a")); print("A:", {k: v for k, v in results["A"].items() if k != "log"}, flush=True)
if a.arm in ("b", "both"):
    results["B"] = arm_b(os.path.join(a.work, "b")); print("B:", json.dumps(results["B"], indent=1)[:3000], flush=True)
if a.arm == "both":
    render(results, a.out)
else:
    json.dump(results, open(os.path.join(a.out, f"results-{a.arm}.json"), "w"), indent=1)
