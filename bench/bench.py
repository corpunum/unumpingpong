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
ap.add_argument("--player", choices=["openai", "claude", "codex"], default="openai",
                help="openai = the local endpoint (reference); claude / codex = the agent CLIs (cloud players)")
ap.add_argument("--label", help="column name for a CLI player in results.json, e.g. haiku or luna-low")
ap.add_argument("--cli-model", help="model passed to the CLI (claude: haiku; codex: gpt-5.6-luna)")
ap.add_argument("--cli-effort", default="", help="codex model_reasoning_effort for arm A (B uses --effort)")
ap.add_argument("--cli-timeout", type=int, default=2400, help="arm A: wall limit of the one agent session, seconds")
ap.add_argument("--b-persistent", type=int, default=1, help="CLI players, B: one long-lived process per table (1) or one call per rally (0)")
ap.add_argument("--b-lean", type=int, default=1, help="CLI players, B: lean context (1) or the v0.2.0 bench isolation flags (0)")
ap.add_argument("--b-tools", default="none", help="CLI players, B: 'none' (apply writes the file, the referee tests), 'default', or a comma list")
ap.add_argument("--b-system-prompt", type=int, default=0, help="CLI players, B: replace the CLI's system prompt with a short player prompt")
ap.add_argument("--a-lean", type=int, default=1, help="CLI players, A: the same lean flags as B, tools limited to Read/Edit/Write/Bash")
ap.add_argument("--b-search", type=int, default=0, help="CLI players, B: attach the UnumSearch MCP server (search = \"unumsearch\")")
ap.add_argument("--arm-key", help="store the result under this key instead of A/B (ablations)")
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


CACHED_WEIGHT = 0.1  # cache reads cost about a tenth of fresh input (Anthropic and OpenAI)


def split(fresh, cached, out, extra=0):
    """Token fields of an arm. `tokens` (the headline) is billable: fresh input + output + cache
    reads at their discount; `tokens_total` is everything the model processed."""
    billable = round(fresh + out + cached * CACHED_WEIGHT) + extra
    return {"fresh_input_tokens": fresh, "cached_input_tokens": cached, "completion_tokens": out,
            "prompt_tokens": fresh + cached, "tokens_total": fresh + cached + out + extra,
            "billable_tokens": billable, "tokens": billable}


def player_modes(db):
    try:
        import sqlite3
        c = sqlite3.connect(db)
        return sorted({json.loads(d)["mode"] for (d,) in c.execute("SELECT data_json FROM events WHERE kind='player_mode'")})
    except Exception:  # noqa: BLE001
        return []


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


# ---------------------------------------------------------------- arm A with an agent CLI
CLI_A_PROMPT = """You are working in a directory with several task folders. Each folder has solution.py (a stub whose
docstring is the specification) and test_solution.py. Make every task's tests pass by editing solution.py
(do not edit the tests). Run `python3 -m unittest -q test_solution` inside a folder to check yourself.
Work through ALL tasks: {tasks}.
Finish your final message with a line that says exactly DONE if every task's tests pass, or NOT DONE: <why> otherwise."""


# Codex features that add tools/context a coding player does not need; the same list as the
# Rust adapter's lean mode (crates/upp-core/src/player/cli.rs).
CODEX_LEAN_FEATURES = ["apps", "browser_use", "browser_use_external", "computer_use", "image_generation", "multi_agent",
                       "goals", "plugins", "remote_plugin", "skill_search", "skill_mcp_dependency_install", "tool_suggest",
                       "view_image", "sleep_tool", "realtime_conversation", "in_app_browser", "workspace_dependencies",
                       "hooks", "memories"]
B_SYSTEM_PROMPT = ("You are the player at an UnumPingPong table. Each message is a task or the referee's report on your "
                   "last answer. Reply with the complete content of the requested file in one fenced code block (it is "
                   "written to disk for you and the tests run after), then the <card> block when asked. Do not use tools.")


def legacy_isolation():
    """The v0.2.0 bench flags (user settings / MCP off), kept for the ablation."""
    if a.player == "claude":
        return ["--setting-sources", "", "--strict-mcp-config", "--dangerously-skip-permissions"]
    return ["--dangerously-bypass-approvals-and-sandbox", "-c", "features.memories=false", "-c", "mcp_servers={}"]


def codex_lean():
    import tomllib
    out = []
    for f in CODEX_LEAN_FEATURES:
        out += ["-c", f"features.{f}=false"]
    out += ["-c", 'web_search="disabled"']
    try:
        cfg = tomllib.load(open(os.path.join(os.environ.get("CODEX_HOME", os.path.expanduser("~/.codex")), "config.toml"), "rb"))
    except Exception:  # noqa: BLE001
        cfg = {}
    for name in cfg.get("mcp_servers", {}):
        out += ["-c", f"mcp_servers.{json.dumps(name) if not re.fullmatch(r'[A-Za-z0-9_-]+', name) else name}.enabled=false"]
    return out


def a_cli_args():
    """Arm A: one agent session; it needs tools to edit files and run the tests."""
    if not a.a_lean:
        return legacy_isolation()
    if a.player == "claude":
        return ["--setting-sources", "", "--strict-mcp-config", "--disable-slash-commands",
                "--exclude-dynamic-system-prompt-sections", "--dangerously-skip-permissions",
                "--tools", "Read,Edit,Write,Bash"]
    return ["--dangerously-bypass-approvals-and-sandbox", *codex_lean()]


def b_player_toml():
    fields = [f'kind = "{a.player}"', f"model = {json.dumps(a.cli_model)}", "timeout_secs = 900",
              f"persistent = {'true' if a.b_persistent else 'false'}", f"lean = {'true' if a.b_lean else 'false'}"]
    if a.b_tools == "none":
        fields.append("tools = []")
    elif a.b_tools != "default":
        fields.append("tools = " + json.dumps(a.b_tools.split(",")))
    if a.b_system_prompt:
        fields.append(f"system_prompt = {json.dumps(B_SYSTEM_PROMPT)}")
    if a.b_search:
        fields.append('search = "unumsearch"')
    if a.b_lean:
        args = ["--dangerously-skip-permissions"] if a.player == "claude" else ["--dangerously-bypass-approvals-and-sandbox"]
    else:
        args = legacy_isolation()
    fields.append(f"args = {json.dumps(args)}")
    return "{ " + ", ".join(fields) + " }"


def b_config():
    return {"persistent": bool(a.b_persistent), "lean": bool(a.b_lean), "tools": a.b_tools,
            "system_prompt": bool(a.b_system_prompt), "search": bool(a.b_search)}


def arm_a_cli(root):
    fresh(root)
    prompt = CLI_A_PROMPT.format(tasks=", ".join(TASKS))
    last = os.path.join(os.path.dirname(root), "last.txt")
    if os.path.exists(last):
        os.remove(last)
    if a.player == "claude":
        argv = ["claude", "-p", "--output-format", "json", "--model", a.cli_model, *a_cli_args()]
    else:
        argv = ["codex", "exec", "--json", "--skip-git-repo-check", "-o", last, "-m", a.cli_model, *a_cli_args()]
        if a.cli_effort:
            argv += ["-c", f'model_reasoning_effort="{a.cli_effort}"']
        argv.append("-")
    t0 = time.time()
    try:
        r = subprocess.run(argv, input=prompt, cwd=root, capture_output=True, text=True, timeout=a.cli_timeout)
        timed_out = False
    except subprocess.TimeoutExpired as e:
        out = e.stdout.decode() if isinstance(e.stdout, bytes) else (e.stdout or "")
        r = subprocess.CompletedProcess(argv, -9, out, "")
        timed_out = True
    wall = time.time() - t0
    fin = pout = cached = 0; cost = 0.0; text = ""; turns = None
    if a.player == "claude":
        try:
            j = json.loads(r.stdout)
            u = j.get("usage", {})
            # Fresh input = input + cache writes; cache reads apart (as the adapter counts them).
            fin = u.get("input_tokens", 0) + u.get("cache_creation_input_tokens", 0)
            cached = u.get("cache_read_input_tokens", 0)
            pout = u.get("output_tokens", 0); cost = j.get("total_cost_usd", 0.0); text = j.get("result", ""); turns = j.get("num_turns")
        except Exception:  # noqa: BLE001
            pass
    else:
        for l in r.stdout.splitlines():
            try:
                v = json.loads(l)
            except Exception:  # noqa: BLE001
                continue
            if v.get("type") == "turn.completed":
                # input_tokens includes the cached part.
                c = v["usage"].get("cached_input_tokens", 0)
                fin += v["usage"].get("input_tokens", 0) - c; cached += c
                pout += v["usage"].get("output_tokens", 0)
            if v.get("type") == "item.completed" and v["item"].get("type") == "agent_message":
                text = v["item"].get("text", "")
        if os.path.exists(last):
            text = open(last).read() or text
    ok = referee(root)
    pts = sum(ok.values())
    declared = bool(re.search(r"^\s*DONE\s*$", text, re.M)) and "NOT DONE" not in text
    return headline({"arm": "A one agent session", "points": pts, "per_task": ok, "turns": turns, "max_turns": None,
                     "shell_runs": None, "wall_s": round(wall, 1), **split(fin, cached, pout),
                     "cost_usd": round(cost, 4) if cost else None, "lean": bool(a.a_lean),
                     "declared_done": declared, "exit_code": r.returncode, "timed_out": timed_out,
                     "claimed_done_but_failed": declared and pts < len(TASKS),
                     "failed_when_claimed_done": (len(TASKS) - pts) if declared else None,
                     "final_message_tail": text[-300:], "stderr_tail": r.stderr[-300:]}, len(TASKS))


# ---------------------------------------------------------------- arms B and C: UnumPingPong tables
def upp_json(*args):
    r = subprocess.run([UPP, *args], capture_output=True, text=True)
    return json.loads(r.stdout) if r.stdout.strip().startswith("{") else {}


def arm_tables(root, with_orchestrator):
    fresh(root)
    effort = [e for e in a.effort.split(",") if e]
    if a.player != "openai":
        player = b_player_toml()
    else:
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
    player_tokens = sum(x.get("billable_tokens", x.get("tokens_in", 0) + x.get("tokens_out", 0)) for x in rows)
    orch_tokens = sum(t.get("orchestrator_tokens", 0) for t in summary.get("tables", []))
    pts = sum(ok.values())
    return headline({
        "arm": "C tables + persistent orchestrator" if with_orchestrator else "B UnumPingPong tables",
        "points": pts, "per_task": ok, "rallies": len(rows), "faults": sum(1 for x in rows if x["verdict"] == "fault"),
        "wall_s": round(wall, 1), "effort": effort, "parallel": a.parallel,
        "parallel_used": re.search(r"--parallel auto -> (\d+)", r.stderr).group(1) if "--parallel auto" in r.stderr else a.parallel,
        **split(sum(x.get("tokens_in", 0) for x in rows), sum(x.get("tokens_cached", 0) for x in rows),
                sum(x.get("tokens_out", 0) for x in rows), orch_tokens),
        "cost_usd": round(sum(x.get("cost_usd", 0) or 0 for x in rows), 6) or None,
        "player_tokens": player_tokens, "orchestrator_tokens": orch_tokens,
        "player_mode": player_modes(db),
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
        "calls": lambda: ((f"{r['turns']} turns" if r.get("turns") else "one session") + (f" ({r['shell_runs']} self-checks)" if r.get("shell_runs") is not None else "") if "turns" in r
                          else f"{r['rallies']} rallies" + (f" ({r['faults']} faults)" if r.get("faults") else "")),
        "false": lambda: ((f"{'yes' if r['claimed_done_but_failed'] else 'no'}: declared DONE with "
                           f"{r['failed_when_claimed_done']} of {r['tasks']} failing") if r.get("declared_done")
                          else ("never declared DONE" if "turns" in r
                                else (f"{r['player_claimed_done_referee_failed']} of {r['block_cards_claiming_done']} "
                                      "player cards claiming done (caught by the referee)")
                                if "block_cards_claiming_done" in r else "n/a (no cards in v0.1)")),
        "cost": lambda: (f"${r['cost_usd']:.3f}" if r.get("cost_usd") else "not reported by the CLI"),
        "split": lambda: (f"player {r['player_tokens']:,} / orchestrator {r['orchestrator_tokens']:,}" if "player_tokens" in r else "n/a"),
    }[key]()


def render(res, out):
    L = ["# Benchmark: single agent loop vs UnumPingPong tables", "",
         f"Model `{res.get('model', '?')}` at a local OpenAI-compatible endpoint (Halogen, 3 slots), temperature "
         f"{res.get('temperature')}, max_tokens {res.get('max_tokens', 6000)} per call. One run per arm. "
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
    CLOUD = {"haiku": "Haiku", "luna-low": "Luna low"}
    k = lambda v: f"{v / 1000:.1f}k" if v is not None else "n/a"
    crows = [("Verified tasks", lambda r: f"{r['points']}/{r['tasks']}"),
             ("Wall time", lambda r: f"{r['wall_s']:.0f} s"),
             ("**Billable tokens** (fresh in + out + 0.1 x cached)", lambda r: f"**{k(r.get('billable_tokens'))}**"),
             ("Fresh input / cached input / output", lambda r: (f"{k(r['fresh_input_tokens'])} / {k(r['cached_input_tokens'])} / "
                                                                f"{k(r['completion_tokens'])}") if "fresh_input_tokens" in r else "n/a"),
             ("All tokens processed", lambda r: k(r.get("tokens_total", r.get("tokens")))),
             ("Cost reported by the CLI", lambda r: f"${r['cost_usd']:.4f}" if r.get("cost_usd") else "none"),
             ("Verified per 100k billable tokens", lambda r: f"{r['points'] / r['billable_tokens'] * 1e5:.1f}" if r.get("billable_tokens") else "n/a"),
             ("Model calls", lambda r: (f"{r['turns']} turns" if r.get("turns") else "one session") if "turns" in r
              else f"{r['rallies']} rallies" + (f" ({r['faults']} faults)" if r.get("faults") else ""))]
    for name, s in res.get("sets", {}).items():
        cl, old = s.get("cloud", {}), s.get("cloud_v020", {})
        if not (cl or old):
            continue
        cols = []
        for lab in CLOUD:
            for src, ver in ((old, "v0.2.0"), (cl, "v0.2.1")):
                for arm in ("A", "B"):
                    if arm in src.get(lab, {}):
                        cols.append((f"{CLOUD[lab]} {arm} {ver}", src[lab][arm]))
        L += [f"## {name.capitalize()} set with cloud players: v0.2.0 vs v0.2.1", "",
              "| Metric | " + " | ".join(c for c, _ in cols) + " |", "| --- |" + " --- |" * len(cols)]
        for label, f in crows:
            L.append(f"| {label} | " + " | ".join(f(r) for _, r in cols) + " |")
        L.append("")
        abl = [(f"{CLOUD.get(lab, lab)} {kk}", v) for lab in CLOUD for kk, v in cl.get(lab, {}).items()
               if kk.startswith("B") and kk != "B" and "config" in v]
        abl += [(f"{CLOUD.get(lab, lab)} B", cl[lab]["B"]) for lab in CLOUD
                if "B" in cl.get(lab, {}) and abl and not cl[lab]["B"].get("note")]
        if abl:
            L += [f"### {name.capitalize()} set: what each change does (B tables, one run each)", "",
                  "| Run | persistent | lean | tools | system prompt | UnumSearch MCP | verified | billable | fresh / cached / out | cost | rallies |",
                  "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
            for kk, v in abl:
                c = v["config"]
                L.append(f"| {kk} | {'yes' if c['persistent'] else 'no'} | {'yes' if c['lean'] else 'no (v0.2.0 flags)'} | "
                         f"{c['tools']} | {'short' if c['system_prompt'] else 'CLI default'} | {'on' if c.get('search') else 'off'} | "
                         f"{v['points']}/{v['tasks']} | "
                         f"{k(v['billable_tokens'])} | {k(v['fresh_input_tokens'])} / {k(v['cached_input_tokens'])} / "
                         f"{k(v['completion_tokens'])} | " + (f"${v['cost_usd']:.4f}" if v.get("cost_usd") else "none") + f" | {v['rallies']} |")
            L.append("")
    L += res.get("notes_md", "").splitlines()
    open(os.path.join(out, "results.md"), "w").write("\n".join(L).rstrip() + "\n")
    print("\n".join(L))


if a.render:
    render(json.load(open(a.render)), a.out); sys.exit(0)
if not a.model and a.player == "openai":
    ap.error("--model is required")

path = os.path.join(a.out, "results.json")
res = json.load(open(path)) if os.path.exists(path) else {}
cli = a.player != "openai"
if cli:
    if not (a.label and a.cli_model):
        ap.error("--player claude|codex needs --label and --cli-model")
    a.model = a.model or a.cli_model
else:
    res.update({"model": a.model, "base_url": a.base_url, "temperature": a.temperature, "max_tokens": a.max_tokens})
s = res.setdefault("sets", {}).setdefault(a.set, {})
s["tasks"] = TASKS
for arm in [x.strip() for x in a.arm.split(",") if x.strip()]:
    st = {} if cli else endpoint_state()
    work = os.path.join(a.work, a.label or "local", a.set, a.arm_key or arm)
    if cli:
        r = arm_a_cli(work) if arm == "a" else arm_tables(work, False)
        r["player"] = a.player; r["cli_model"] = a.cli_model
        r["effort"] = r.get("effort") or ([a.cli_effort] if a.cli_effort else [])
        if arm != "a":
            r["config"] = b_config()
    else:
        r = arm_a(work) if arm == "a" else arm_tables(work, arm == "c")
    r["date"] = time.strftime("%Y-%m-%d"); r["endpoint_at_start"] = st
    (s.setdefault("cloud", {}).setdefault(a.label, {}) if cli else s)[a.arm_key or arm.upper()] = r
    print(a.label or "local", arm.upper(), json.dumps({k: v for k, v in r.items() if k not in ("log", "per_table", "upp_stderr_tail")}), flush=True)
    json.dump(res, open(path, "w"), indent=1)
render(res, a.out)
