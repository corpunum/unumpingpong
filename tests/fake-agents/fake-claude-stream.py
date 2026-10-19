#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Stand-in for `claude -p --input-format stream-json --output-format stream-json`: one process,
one user message per line on stdin, one `result` line per message on stdout.

  FAKE_LOG      directory: argv of every start goes to spawns.log; every turn's message to
                turn-<n>.txt (n counts across processes)
  FAKE_ANSWERS  answers by turn, separated by '|' (the last repeats)   default: 42
Usage per turn: input 10, cache writes 100, cache reads 1000, output 5. total_cost_usd is
cumulative for the process (0.01 per turn), like the real CLI.
"""
import json, os, sys

log = os.environ.get("FAKE_LOG", "fake-log")
os.makedirs(log, exist_ok=True)
argv = sys.argv[1:]
with open(os.path.join(log, "spawns.log"), "a") as f:
    f.write(" ".join(argv) + "\n")
if "--input-format" not in argv or argv[argv.index("--input-format") + 1] != "stream-json":
    print("error: this fake only speaks stream-json", file=sys.stderr)
    sys.exit(2)
session = argv[argv.index("--resume") + 1] if "--resume" in argv else "sess-1"
answers = os.environ.get("FAKE_ANSWERS", "42").split("|")
counter = os.path.join(log, "turns")
cost = 0.0
for line in sys.stdin:
    if not line.strip():
        continue
    msg = json.loads(line)
    n = int(open(counter).read()) + 1 if os.path.exists(counter) else 1
    open(counter, "w").write(str(n))
    with open(os.path.join(log, f"turn-{n}.txt"), "w") as f:
        f.write(msg["message"]["content"])
    answer = answers[min(n, len(answers)) - 1]
    with open("answer.txt", "w") as f:
        f.write(answer + "\n")
    cost += 0.01
    print(json.dumps({"type": "system", "subtype": "init", "session_id": session}), flush=True)
    print(json.dumps({"type": "assistant", "message": {"content": [{"type": "text", "text": answer}]}}), flush=True)
    print(json.dumps({
        "type": "result", "subtype": "success", "is_error": False, "result": f"wrote {answer}",
        "session_id": session, "total_cost_usd": round(cost, 6),
        "usage": {"input_tokens": 10, "cache_creation_input_tokens": 100,
                  "cache_read_input_tokens": 1000, "output_tokens": 5},
    }), flush=True)
