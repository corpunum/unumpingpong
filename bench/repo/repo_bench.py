#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Real-repository benchmark: one agent session (A) vs UnumPingPong tables (B) on real bug fixes.

Every task is a real fix commit from a real repository (bench/repo/tasks.json): the checkout is
the commit's parent plus the fix's own unit test, which fails there. The task statement is the
issue title and the failing test file; the source file to change is NOT named. A task counts only
when, afterwards, the external referee passes:

    npx vitest run <test file>    (exit 0)   and   the test file is unchanged

  A  one agent session gets all tasks at once (one folder each) and works through them alone.
  B  one UnumPingPong table per task; the referee runs after every answer and its output goes
     back to the same player session (max 3 rallies); tables run in parallel.
  C  the same tables sharing ONE player session (`share`), one table at a time: B's referee loop
     with A's session reuse (for players whose provider caches a growing context cheaply).

Players: qwen (local Qwen through OpenUnum's agent, `openunum` player), haiku (Claude Code CLI),
luna-low (Codex CLI, reasoning effort low). Standard library only, plus `upp`, git and node.

    python3 bench/repo/repo_bench.py --player haiku --arm a,b
    python3 bench/repo/repo_bench.py --render          # rewrite results.md only
"""
import argparse, json, os, shlex, shutil, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
SPEC = json.load(open(os.path.join(HERE, "tasks.json")))
TASKS = SPEC["tasks"]

ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
ap.add_argument("--player", choices=["qwen", "haiku", "luna-low", "opencode", "luna-openunum"])
ap.add_argument("--model", help="opencode: provider/model, e.g. opencode/nemotron-3-ultra-free or nim/openai/gpt-oss-20b")
ap.add_argument("--label", help="results column for an opencode model (default: derived from --model)")
ap.add_argument("--arm", default="a,b")
ap.add_argument("--repo", default=SPEC["repo_dir_default"], help="local clone of the task repository")
ap.add_argument("--node-modules", default=None, help="node_modules to link into every checkout (default: <repo>/node_modules)")
ap.add_argument("--work", default="/tmp/upp-repo-bench")
ap.add_argument("--upp", default=os.path.join(ROOT, "target", "release", "upp"))
ap.add_argument("--openunum-url", default="http://127.0.0.1:18880")
ap.add_argument("--a-timeout", type=int, default=3600, help="arm A wall limit (s)")
ap.add_argument("--b-rallies", type=int, default=3)
ap.add_argument("--b-wall", type=int, default=1500, help="arm B wall lease per table (s)")
ap.add_argument("--parallel", type=int, default=0, help="arm B tables at once (default: 2 for qwen, 4 for cloud)")
ap.add_argument("--only", help="comma list of task ids (smoke runs)")
ap.add_argument("--system-prompt", help="codex/claude players: replace the CLI's own system prompt (codex: baseInstructions)")
ap.add_argument("--render", action="store_true")
a = ap.parse_args()
if a.only:
    TASKS = [t for t in TASKS if t["id"] in a.only.split(",")]
RESULTS = os.path.join(HERE, "results.json")
OC_SANDBOX = os.path.join(HERE, "oc-sandbox.sh")
CODEX_SANDBOX = os.path.join(HERE, "codex-sandbox.sh")
NM = a.node_modules or os.path.join(a.repo, "node_modules")
UPP = os.path.abspath(a.upp)
KEY = a.label or (("oc:" + a.model.split("/", 1)[-1]) if a.player == "opencode" and a.model else a.player)
if a.player == "opencode":
    if not a.model:
        sys.exit("--model is required for --player opencode")
    if a.model.startswith("nim/") and not os.environ.get("NVIDIA_API_KEY"):
        # opencode's NIM provider reads {env:NVIDIA_API_KEY}; take it from OpenUnum's secrets, never print it.
        try:
            os.environ["NVIDIA_API_KEY"] = json.load(open(os.path.expanduser("~/.openunum/secrets.json")))["secrets"]["nvidiaApiKey"]
        except Exception:  # noqa: BLE001
            sys.exit("NVIDIA_API_KEY not set and not found in ~/.openunum/secrets.json")
# Fresh OpenUnum sessions per run: an old session with the same id would carry its history.
RUN_TAG = time.strftime("%m%d%H%M")


def sh(cmd, cwd=None, timeout=600):
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)


# ---------------------------------------------------------------- task checkouts and the referee
def setup(root):
    os.environ["UPP_SANDBOX_LABEL"] = root  # every sandbox container of this run carries it
    """One git worktree per task at the fix's parent, plus the fix's failing test (committed, so any
    later edit to it shows up in `git diff`)."""
    teardown(root)
    os.makedirs(root)
    for t in TASKS:
        d = os.path.join(root, t["id"])
        # A fresh repository from `git archive` of the parent: no history, so the fix commit (and
        # every later commit) cannot be looked up. A worktree would share the whole object store.
        os.makedirs(d)
        arc = subprocess.run(["git", "-C", a.repo, "archive", t["base"]], capture_output=True)
        subprocess.run(["tar", "-x", "-C", d], input=arc.stdout, check=True)
        # Old checkouts predate OpenUnum's host guard (115ef1dc): a full unit-suite run there can
        # restart the REAL model server via `systemctl --user restart` (live 10-20: three Halogen
        # restarts in 6 min). Cut every test process off the user systemd bus.
        setup_file = os.path.join(d, "tests", "setup", "force-temp-home.setup.mjs")
        if os.path.exists(setup_file):
            with open(setup_file, "a") as f:
                f.write("\n// upp bench sandbox: no test may reach the host's systemd user bus.\n"
                        "process.env.XDG_RUNTIME_DIR = '/nonexistent-upp-bench';\n"
                        "process.env.DBUS_SESSION_BUS_ADDRESS = 'unix:path=/nonexistent-upp-bench/bus';\n")
        # It installs a real OpenUnum runtime and starts a server; in old checkouts that server
        # outlived the test and its self-heal restarted the host's model unit (live 10-20).
        e2e = os.path.join(d, "tests", "unit", "install-end-to-end.test.mjs")
        if os.path.exists(e2e) and t["test"] != "tests/unit/install-end-to-end.test.mjs":
            os.remove(e2e)
        sh(["git", "init", "-q"], cwd=d)
        sh(["git", "add", "-A"], cwd=d)
        sh(["git", "-c", "user.name=bench", "-c", "user.email=bench@local", "commit", "-q", "-m", "base"], cwd=d)
        src = sh(["git", "-C", a.repo, "show", f'{t["commit"]}:{t["test"]}']).stdout
        os.makedirs(os.path.dirname(os.path.join(d, t["test"])), exist_ok=True)
        open(os.path.join(d, t["test"]), "w").write(src)
        sh(["git", "add", t["test"]], cwd=d)
        sh(["git", "-c", "user.name=bench", "-c", "user.email=bench@local", "commit", "-q", "-m", "task: failing test"], cwd=d)
        os.symlink(NM, os.path.join(d, "node_modules"))
        # The referee lives OUTSIDE the checkout: an agent can neither edit nor delete it
        # (10-20: a `git clean`/`stash -u` in a checkout removed an in-tree check.sh).
        chk = check_path(root, t)
        os.makedirs(os.path.dirname(chk), exist_ok=True)
        open(chk, "w").write(
            "#!/bin/sh\n# referee: the task's test passes and is unchanged (run with cwd = the checkout)\n"
            f"git diff --quiet HEAD -- {t['test']} || {{ echo 'the test file was modified'; exit 1; }}\n"
            f"exec timeout 300 npx vitest run {t['test']}\n")
        os.chmod(chk, 0o755)


def check_path(root, t):
    # Beside the run folder, not inside it: the solo arm's sandbox mounts the whole run folder.
    return os.path.join(root + ".referee", t["id"] + ".sh")


def stop_sandboxes(root):
    """Stop sandbox containers of this run: killing a `podman run` client leaves its container running."""
    ids = sh(["podman", "ps", "-q", "--filter", f"label=upp-sandbox={root}"]).stdout.split()
    if ids:
        sh(["podman", "stop", "-t", "5", *ids], timeout=120)


def teardown(root):
    stop_sandboxes(root)
    if not os.path.isdir(root):
        return
    for n in os.listdir(root):
        try:
            os.remove(os.path.join(root, n, "node_modules"))  # a symlink: never follow it
        except OSError:
            pass
    shutil.rmtree(root, ignore_errors=True)
    shutil.rmtree(root + ".referee", ignore_errors=True)


def referee(root):
    out = {}
    for t in TASKS:
        try:
            out[t["id"]] = sh(["sh", check_path(root, t)], cwd=os.path.join(root, t["id"]), timeout=400).returncode == 0
        except subprocess.TimeoutExpired:
            out[t["id"]] = False
    return out


def statement(t, d=None):
    # The openunum player runs inside OpenUnum's own workspace, not in the table's cwd: name the
    # checkout explicitly (live 10-20: without it the agent searched the owner's real clone).
    where = (f"The checkout for this task is {d} -- work ONLY inside that directory (cd there first); "
             "do not read or change any other repository.\n") if d else ""
    return (where + f'Repository task (real bug, {t["repo"]}): "{t["issue"]}".\n'
            f'The unit test `{t["test"]}` describes the expected behaviour and currently fails. '
            "Find the cause in the source and fix it so the test passes. Do not edit or delete the test file. "
            "Run only this test file, never the whole suite. "
            f'Check yourself with `npx vitest run {t["test"]}`.')


# ---------------------------------------------------------------- players
LEAN_CLAUDE = ["--setting-sources", "", "--strict-mcp-config", "--disable-slash-commands",
               "--exclude-dynamic-system-prompt-sections", "--dangerously-skip-permissions", "--tools", "Read,Edit,Write,Bash,Grep,Glob"]
CODEX_LEAN_FEATURES = ["apps", "browser_use", "browser_use_external", "computer_use", "image_generation", "multi_agent",
                       "goals", "plugins", "remote_plugin", "skill_search", "skill_mcp_dependency_install", "tool_suggest",
                       "view_image", "sleep_tool", "realtime_conversation", "in_app_browser", "workspace_dependencies",
                       "hooks", "memories"]


def codex_lean():
    out = []
    for f in CODEX_LEAN_FEATURES:
        out += ["-c", f"features.{f}=false"]
    return out + ["-c", 'web_search="disabled"', "-c", "mcp_servers={}"]


def player_toml():
    if a.player == "qwen":
        return ('{ kind = "openunum", base_url = ' + json.dumps(a.openunum_url) +
                ', session_prefix = "upp-repo-' + RUN_TAG + '-", timeout_secs = 1500 }')
    if a.player == "haiku":
        return ('{ kind = "claude", model = "haiku", persistent = true, lean = true, timeout_secs = 1500, '
                'tools = ["Read","Edit","Write","Bash","Grep","Glob"], args = ["--dangerously-skip-permissions"] }')
    if a.player == "luna-openunum":
        # Luna in OpenUnum's own agent loop: one OpenUnum worker per rally (openunum-luna-player.py).
        return ('{ kind = "shell", command = ["python3", ' + json.dumps(os.path.join(HERE, "openunum-luna-player.py"))
                + '], timeout_secs = 1700 }')
    if a.player == "opencode":
        # --auto gives opencode every permission: run it in the podman sandbox (oc-sandbox.sh).
        return ('{ kind = "opencode", bin = ' + json.dumps(OC_SANDBOX) + ', model = ' + json.dumps(a.model)
                + ', timeout_secs = 1500, args = ["--auto"] }')
    # Bypassed approvals inside the podman sandbox (codex's own bubblewrap sandbox cannot start here).
    sp = (', system_prompt = ' + json.dumps(a.system_prompt)) if a.system_prompt else ''
    return ('{ kind = "codex", bin = ' + json.dumps(CODEX_SANDBOX) + ', model = "gpt-5.6-luna", persistent = true, lean = true, '
            'timeout_secs = 1500, args = ["--dangerously-bypass-approvals-and-sandbox"]' + sp + ' }')


# ---------------------------------------------------------------- arm A: one agent session
def arm_a(root):
    setup(root)
    folders = "\n".join(f'- {os.path.join(root, t["id"])}/: {statement(t)}' for t in TASKS)
    prompt = ("You are working in a directory with one folder per task; each folder is a checkout of the same "
              "JavaScript repository. Fix every task inside its own folder (run commands from that folder).\n"
              f"{folders}\n"
              "Work through ALL tasks. Finish with a line that says exactly DONE if every task's test passes, "
              "or NOT DONE: <why> otherwise.")
    fin = cached = pout = 0
    cost = None
    t0 = time.time()
    timed_out = False
    if a.player == "qwen":
        # One OpenUnum session for everything: a single table, one rally, no referee feedback.
        match = os.path.join(root, "a.toml")
        open(match, "w").write(f'[[table]]\nname = "solo"\nball = {json.dumps(prompt)}\ncwd = {json.dumps(root)}\n'
                               f'player = {player_toml()}\nreferee = [{{ kind = "command", run = "true" }}]\n'
                               f'lease = {{ wall_secs = {a.a_timeout}, max_rallies = 1 }}\n')
        r = sh([UPP, "--db", os.path.join(root, "a.db"), "run", match], timeout=a.a_timeout + 120)
    else:
        if a.player == "opencode":
            argv = [OC_SANDBOX, "run", "--format", "json", "--auto", "--dir", root, "-m", a.model, prompt]
        elif a.player == "haiku":
            argv = ["claude", "-p", "--output-format", "json", "--model", "haiku", *LEAN_CLAUDE]
        else:
            argv = [CODEX_SANDBOX, "exec", "--json", "--skip-git-repo-check", "-m", "gpt-5.6-luna",
                    "--dangerously-bypass-approvals-and-sandbox", *codex_lean(), "-c", 'model_reasoning_effort="low"', "-"]
        try:
            # PWD too: opencode trusts $PWD over the process cwd (10-20: an agent wandered into $HOME).
            r = subprocess.run(argv, input=None if a.player == "opencode" else prompt, cwd=root,
                               env={**os.environ, "PWD": root}, capture_output=True, text=True, timeout=a.a_timeout)
        except subprocess.TimeoutExpired as e:
            r = subprocess.CompletedProcess(argv, -9, e.stdout.decode() if isinstance(e.stdout, bytes) else (e.stdout or ""), "")
            timed_out = True
        if a.player == "opencode":
            for line in r.stdout.splitlines():
                try:
                    v = json.loads(line)
                except Exception:  # noqa: BLE001
                    continue
                if v.get("type") == "step_finish":
                    tk = v.get("part", {}).get("tokens", {}) or {}
                    c = (tk.get("cache") or {})
                    fin += (tk.get("input", 0) or 0) + (c.get("write", 0) or 0)
                    cached += c.get("read", 0) or 0
                    pout += (tk.get("output", 0) or 0) + (tk.get("reasoning", 0) or 0)
                    cost = (cost or 0) + (v.get("part", {}).get("cost", 0) or 0)
        elif a.player == "haiku":
            try:
                j = json.loads(r.stdout)
                u = j.get("usage", {})
                fin = u.get("input_tokens", 0) + u.get("cache_creation_input_tokens", 0)
                cached = u.get("cache_read_input_tokens", 0)
                pout = u.get("output_tokens", 0)
                cost = j.get("total_cost_usd")
            except Exception:  # noqa: BLE001
                pass
        else:
            for line in r.stdout.splitlines():
                try:
                    v = json.loads(line)
                except Exception:  # noqa: BLE001
                    continue
                if v.get("type") == "turn.completed":
                    c = v["usage"].get("cached_input_tokens", 0)
                    fin += v["usage"].get("input_tokens", 0) - c
                    cached += c
                    pout += v["usage"].get("output_tokens", 0)
    wall = time.time() - t0
    stop_sandboxes(root)
    ok = referee(root)
    teardown(root)
    return result("A one agent session", ok, wall, fin, cached, pout, cost, {"timed_out": timed_out})


# ---------------------------------------------------------------- arm B: UnumPingPong tables
def arm_b(root, shared=False):
    setup(root)
    # opencode keeps its state in one SQLite db: one session at a time is the safe setting.
    # C = the same tables sharing ONE player session, one table at a time.
    par = 1 if shared else a.parallel or (2 if a.player == "qwen" else 1 if a.player == "opencode" else 4)
    lines = [f'[defaults]\nplayer = {player_toml()}\nlease = {{ wall_secs = {a.b_wall}, max_rallies = {a.b_rallies} }}\n'
             + ('share = "bench"\n' if shared else '')]
    for t in TASKS:
        d = os.path.join(root, t["id"])
        lines.append(f'[[table]]\nname = {json.dumps(t["id"])}\ncwd = {json.dumps(d)}\nball = {json.dumps(statement(t, d if a.player == "qwen" else None))}\n'
                     f'referee = [{{ kind = "command", run = {json.dumps("sh " + shlex.quote(check_path(root, t)))} }}]\n')
    match = os.path.join(root, "b.toml")
    open(match, "w").write("\n".join(lines))
    db = os.path.join(root, "b.db")
    if shared:
        os.environ["UPP_SANDBOX_ROOT"] = root  # one process serves every task folder
    else:
        os.environ.pop("UPP_SANDBOX_ROOT", None)
    t0 = time.time()
    sh([UPP, "--db", db, "run", match, "--parallel", str(par)], timeout=a.b_wall * (len(TASKS) // par + 2))
    wall = time.time() - t0
    rows = []
    try:
        rows = json.loads(sh([UPP, "--db", db, "list", "--all", "--json"]).stdout).get("tables", [])
    except Exception:  # noqa: BLE001
        pass
    billable = sum(r.get("tokens", 0) or 0 for r in rows)  # upp's billable count (fresh + out + 0.1 x cached)
    cost = sum(r.get("cost_usd", 0) or 0 for r in rows) or None
    rallies = sum(r.get("rallies", 0) or 0 for r in rows)
    stop_sandboxes(root)
    ok = referee(root)
    teardown(root)
    return result("C tables, one shared session" if shared else "B UnumPingPong tables", ok, wall, 0, 0, 0, cost, {"parallel": par, "rallies": rallies,
                                                                     "points_by_upp": sum(r.get("status") == "point" for r in rows)},
                  billable=billable)


def result(arm, ok, wall, fin, cached, pout, cost, extra, billable=None):
    if billable is None:
        billable = round(fin + pout + 0.1 * cached)
    pts = sum(ok.values())
    return {"arm": arm, "points": pts, "tasks": len(ok), "per_task": ok, "wall_s": round(wall, 1),
            "tokens": {"fresh_in": fin, "cached_in": cached, "out": pout, "billable": billable},
            "cost_usd": round(cost, 4) if cost else None,
            "verified_per_hour": round(pts / (wall / 3600), 2) if wall else None, **extra}


# ---------------------------------------------------------------- results
def render(res):
    L = ["# Real-repository benchmark", "",
         f"{len(SPEC['tasks'])} real bug fixes from OpenUnum (`{SPEC['tasks'][0]['repo']}`, a private repository; the harness works on any repository): "
         "each task is a fresh repository made from the fix's parent commit (`git archive`, no history, so the fix cannot be looked up) plus the fix's own unit test (which fails there). The player gets the "
         "issue title and the failing test, not the file to change. A task counts only if the test passes afterwards "
         "and the test file is unchanged. Tasks: [tasks.json](tasks.json). One run per cell.", "",
         "| Player | Arm | Verified | Wall | Verified / hour | Billable tokens | Cost | Rallies |",
         "|---|---|---|---|---|---|---|---|"]
    for p, arms in res.get("players", {}).items():
        for k in ("A", "B", "C"):
            c = arms.get(k)
            if not c:
                continue
            tok = c["tokens"]["billable"]
            L.append(f"| {p} | {c['arm']} | {c['points']}/{c['tasks']} | {c['wall_s'] / 60:.1f} min | {c['verified_per_hour']} | "
                     f"{f'{tok:,}' if tok else 'not recorded'} | {('$%.3f' % c['cost_usd']) if c.get('cost_usd') else '-'} | {c.get('rallies', '-')} |")
    L += ["", "Billable tokens = fresh input + output + 0.1 x cached input. \"not recorded\": the `openunum` player "
          "(local Qwen, and Luna inside the OpenUnum agent loop) did not report tokens in these runs.", "",
          "Caveat: runs before 2026-10-20 18:50 UTC kept the referee script inside the agent's checkout, so "
          "tampering with it was not ruled out for them; later runs keep it outside. The opencode and Codex "
          "players ran in a rootless podman sandbox that can write only the task folder "
          "([agent-sandbox.sh](agent-sandbox.sh)).", ""]
    for p, arms in res.get("players", {}).items():
        for k in ("A", "B", "C"):
            c = arms.get(k)
            if c:
                miss = [t for t, v in c["per_task"].items() if not v]
                L.append(f"- {p} {k}: missed {', '.join(miss) if miss else 'none'}")
    open(os.path.join(HERE, "results.md"), "w").write("\n".join(L) + "\n")


def main():
    res = json.load(open(RESULTS)) if os.path.exists(RESULTS) else {"players": {}}
    if a.render:
        return render(res)
    if not a.player:
        sys.exit("--player is required")
    for arm in [x.strip().upper() for x in a.arm.split(",") if x.strip()]:
        root = os.path.join(a.work, f"{KEY.replace(':', '_').replace('/', '_')}-{arm}")
        out = arm_a(root) if arm == "A" else arm_b(root, shared=(arm == "C"))
        out["ran_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        # Several players may run at once: reload and merge right before writing.
        res = json.load(open(RESULTS)) if os.path.exists(RESULTS) else {"players": {}}
        out["model"] = a.model if a.player == "opencode" else None
        res.setdefault("players", {}).setdefault(KEY, {})[arm] = out
        tmp = RESULTS + f".{os.getpid()}.tmp"
        json.dump(res, open(tmp, "w"), indent=1)
        os.replace(tmp, RESULTS)
        print(KEY, arm, out["points"], "/", out["tasks"], f'{out["wall_s"]}s', out["tokens"]["billable"], flush=True)
    render(res)


main()
