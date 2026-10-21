#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Token usage of the `luna-openunum` player, read from OpenUnum's own log.

The shell player reports no tokens, but OpenUnum logs every model call: `inference_request`
(with the session id) and `inference_complete` (with the provider's usage), joined by requestId.
This sums the calls of every worker spawned under the bench parent session in a time window and,
with --label, writes the totals into results.json for that player's arm.

    python3 bench/repo/openunum_tokens.py --since 2026-10-21T03:40:00 [--until ...] \
        [--label "luna (OpenUnum agent loop) r2" --arm B]

Billable = fresh input + output + 0.1 x cached input (the same formula as repo_bench.py).
Standard library only.
"""
import argparse, json, os

ap = argparse.ArgumentParser()
ap.add_argument("--log", default=os.path.expanduser("~/.openunum/logs/openunum.log"))
ap.add_argument("--parent", default="upp-bench-luna-parent")
ap.add_argument("--since", required=True, help="UTC, e.g. 2026-10-21T03:40:00")
ap.add_argument("--until", default="9999")
ap.add_argument("--label")
ap.add_argument("--arm", default="B")
a = ap.parse_args()

workers, req_session, totals = set(), {}, {"calls": 0, "input": 0, "cached_in": 0, "output": 0, "no_cache_calls": 0}
with open(a.log, errors="replace") as f:
    for line in f:
        if '"ts":"' not in line or ("subagent_spawned" not in line and "inference_" not in line):
            continue
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if not (a.since <= e.get("ts", "")[:19] <= a.until):
            continue
        msg = e.get("message")
        if msg == "subagent_spawned" and a.parent in (e.get("parentId"), e.get("rootId")):
            workers.add(e.get("sessionId"))
        elif msg == "inference_request" and e.get("sessionId") in workers:
            req_session[e["requestId"]] = e["sessionId"]
        elif msg == "inference_complete" and e.get("requestId") in req_session and e.get("usage"):
            u = e["usage"]
            totals["calls"] += 1
            totals["input"] += u.get("input") or 0
            totals["cached_in"] += u.get("cachedInput") or 0
            totals["output"] += u.get("output") or 0
            totals["no_cache_calls"] += 0 if u.get("cachedInput") else 1

fresh = totals["input"] - totals["cached_in"]
tokens = {"fresh_in": fresh, "cached_in": totals["cached_in"], "output": totals["output"],
          "billable": round(fresh + totals["output"] + 0.1 * totals["cached_in"])}
print(json.dumps({"workers": len(workers), **totals, "tokens": tokens}))
if a.label:
    p = os.path.join(os.path.dirname(os.path.abspath(__file__)), "results.json")
    res = json.load(open(p))
    cell = res["players"][a.label][a.arm]
    cell["tokens"] = tokens
    cell["model_calls"] = totals["calls"]
    json.dump(res, open(p, "w"), indent=1)
    print(f"wrote tokens into results.json: {a.label} {a.arm}")
