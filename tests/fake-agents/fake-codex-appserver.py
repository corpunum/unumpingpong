#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Stand-in for `codex app-server` (JSON-RPC over stdio): initialize, thread/start or
thread/resume, then one turn/start per rally on the same thread.

  FAKE_LOG        directory: argv of every start goes to spawns.log; thread params to
                  thread.log; every turn's text and effort to turn-<n>.json
  FAKE_ANSWERS    answers by turn, separated by '|' (the last repeats)   default: 42
  FAKE_FAIL_TURN  a turn number that ends with status "failed"
  FAKE_NO_SERVER  when set, `app-server` is an unknown command (exit 2), as in old versions
Anything but `app-server` behaves like a minimal `codex exec --json` (the fallback path).
Usage per model call: input 1000 (800 of them cached), output 20.
"""
import json, os, sys

log = os.environ.get("FAKE_LOG", "fake-log")
os.makedirs(log, exist_ok=True)
argv = sys.argv[1:]
with open(os.path.join(log, "spawns.log"), "a") as f:
    f.write(" ".join(argv) + "\n")
answers = os.environ.get("FAKE_ANSWERS", "42").split("|")
counter = os.path.join(log, "turns")


def next_turn():
    n = int(open(counter).read()) + 1 if os.path.exists(counter) else 1
    open(counter, "w").write(str(n))
    return n


def send(o):
    print(json.dumps(o), flush=True)


if not argv or argv[0] != "app-server" or os.environ.get("FAKE_NO_SERVER"):
    if argv and argv[0] == "app-server":
        print("error: unrecognized subcommand 'app-server'", file=sys.stderr)
        sys.exit(2)
    # exec mode
    sys.stdin.read()
    n = next_turn()
    answer = answers[min(n, len(answers)) - 1]
    open("answer.txt", "w").write(answer + "\n")
    send({"type": "thread.started", "thread_id": "th-exec"})
    send({"type": "item.completed", "item": {"type": "agent_message", "text": f"wrote {answer}"}})
    send({"type": "turn.completed", "usage": {"input_tokens": 1000, "cached_input_tokens": 800, "output_tokens": 20}})
    sys.exit(0)

thread = None
for line in sys.stdin:
    if not line.strip():
        continue
    m = json.loads(line)
    method, mid, p = m.get("method"), m.get("id"), m.get("params") or {}
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {"userAgent": "fake"}})
    elif method in ("thread/start", "thread/resume"):
        thread = p.get("threadId", "th-1")
        with open(os.path.join(log, "thread.log"), "a") as f:
            f.write(json.dumps({"method": method, "params": p}) + "\n")
        send({"jsonrpc": "2.0", "id": mid, "result": {"thread": {"id": thread}}})
        send({"jsonrpc": "2.0", "method": "thread/started", "params": {"thread": {"id": thread}}})
    elif method == "turn/start":
        n = next_turn()
        text = p["input"][0]["text"]
        with open(os.path.join(log, f"turn-{n}.json"), "w") as f:
            json.dump({"text": text, "effort": p.get("effort"), "thread": p["threadId"]}, f)
        answer = answers[min(n, len(answers)) - 1]
        open("answer.txt", "w").write(answer + "\n")
        tid = f"turn-{n}"
        send({"jsonrpc": "2.0", "id": mid, "result": {"turn": {"id": tid, "status": "inProgress"}}})
        # An approval request the client must answer (declined: the player is unattended).
        send({"jsonrpc": "2.0", "id": 900 + n, "method": "item/commandExecution/requestApproval",
              "params": {"threadId": thread, "turnId": tid}})
        reply = json.loads(sys.stdin.readline())
        with open(os.path.join(log, f"approval-{n}.json"), "w") as f:
            json.dump(reply, f)
        send({"jsonrpc": "2.0", "method": "item/completed",
              "params": {"threadId": thread, "turnId": tid,
                         "item": {"type": "agentMessage", "id": "m", "text": f"wrote {answer}"}}})
        usage = {"inputTokens": 1000, "cachedInputTokens": 800, "outputTokens": 20,
                 "reasoningOutputTokens": 0, "totalTokens": 1020}
        send({"jsonrpc": "2.0", "method": "thread/tokenUsage/updated",
              "params": {"threadId": thread, "turnId": tid, "tokenUsage": {"last": usage, "total": usage}}})
        failed = os.environ.get("FAKE_FAIL_TURN") == str(n)
        turn = {"id": tid, "status": "failed" if failed else "completed",
                "error": {"message": "stream disconnected before completion"} if failed else None}
        send({"jsonrpc": "2.0", "method": "turn/completed", "params": {"threadId": thread, "turn": turn}})
