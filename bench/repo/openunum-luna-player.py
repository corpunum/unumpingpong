#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""A `shell` player that runs each ball as an OpenUnum Luna worker (OpenUnum's own agent loop).

The ball arrives on stdin and the player's cwd is the table's checkout. It spawns one OpenUnum
subagent (model "luna", thinking "low") under a dedicated parent session, waits for it to end,
and prints its result card. Each rally is a fresh worker, so UnumPingPong repeats the brief with
the referee's output (a shell player keeps no session).

Env: OPENUNUM_URL (default http://127.0.0.1:18880), UPP_LUNA_PARENT (default upp-bench-luna-parent),
UPP_LUNA_WALL_MS (default 1500000). Standard library only.
"""
import json, os, sys, time, urllib.request

BASE = os.environ.get("OPENUNUM_URL", "http://127.0.0.1:18880")
PARENT = os.environ.get("UPP_LUNA_PARENT", "upp-bench-luna-parent")
WALL = int(os.environ.get("UPP_LUNA_WALL_MS", "1500000"))
TERMINAL = {"done", "failed", "timeout", "partial", "cancelled"}


def call(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method, headers={"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


ball = sys.stdin.read()
cwd = os.getcwd()
call("POST", "/api/sessions", {"sessionId": PARENT})  # idempotent: creates the parent if missing
task = (f"Work ONLY in the git checkout {cwd}: cd there before every command and do not touch any other "
        f"directory or repository.\n\n{ball}")
spawn = call("POST", "/api/tool/run", {
    "name": "session_spawn", "sessionId": PARENT, "summarizeResult": False,
    "args": {"task": task, "handoff": f"Checkout: {cwd}. Done = the named unit test passes and the test file is unchanged. "
             "Reply in 2-4 lines: what you changed and the test result.",
             "model": "luna", "thinking": "low", "title": f"upp luna: {os.path.basename(cwd)}",
             "implementationWorktree": cwd,  # OpenUnum checks writes against this root
             # refereeOwned: the table's referee decides "done"; OpenUnum must not keep continuing it.
             "budget": {"wallTimeMs": WALL, "refereeOwned": True}}})
res = spawn.get("result", spawn)
child = res.get("sessionId") or res.get("childSessionId")
if not child:
    sys.exit(f"spawn failed: {json.dumps(spawn)[:400]}")
deadline = time.time() + WALL / 1000 + 120
while time.time() < deadline:
    tree = call("GET", f"/api/sessions/{child}/tree?maxNodes=1")
    nodes = tree.get("tree") or tree.get("nodes") or []
    node = next((n for n in nodes if n.get("id") == child), nodes[0] if nodes else {})
    if node.get("status") in TERMINAL:
        out = node.get("result_json") or node.get("result")
        try:
            out = json.loads(out).get("summary", out) if isinstance(out, str) else (out or {}).get("summary", out)
        except Exception:  # noqa: BLE001
            pass
        print(f"[openunum luna worker {child}: {node.get('status')}]\n{out}")
        sys.exit(0)  # the referee, not the worker status, decides
    time.sleep(10)
sys.exit(f"worker {child} did not finish in time")
