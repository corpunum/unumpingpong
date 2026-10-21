#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""A `shell` player that runs each ball through nanocode (github.com/gakonst/nanocodex) headless.

The ball arrives on stdin and the player's cwd is the table's checkout. The first rally starts a
nanocode session in that directory; later rallies continue it (`exec -c`), so the referee's output
lands in the same conversation, as with the native Codex and Claude players. Runs inside the podman
sandbox (nanocode-sandbox.sh) with a private home per table under $UPP_NANOCODE_HOMES, where
nanocode also writes its traces (token usage is summed from them afterwards).

Env: UPP_NANOCODE_HOMES (required), UPP_NANOCODE_MODEL (default gpt-6-luna),
UPP_NANOCODE_THINKING (default low). Standard library only.
"""
import os, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
cwd = os.getcwd()
home = os.path.join(os.environ["UPP_NANOCODE_HOMES"], os.path.basename(cwd))
started = os.path.exists(os.path.join(home, ".nanocode", "sessions.jsonl"))
argv = [os.path.join(HERE, "nanocode-sandbox.sh"), "exec", "--no-subagents", *(["-c"] if started else []),
        "-m", os.environ.get("UPP_NANOCODE_MODEL", "gpt-6-luna"),
        "-t", os.environ.get("UPP_NANOCODE_THINKING", "low"), "-C", cwd, sys.stdin.read()]
r = subprocess.run(argv, env={**os.environ, "UPP_SANDBOX_HOME": home, "PWD": cwd}, capture_output=True, text=True)
print(r.stdout[-6000:] or r.stderr[-2000:])
sys.exit(0)  # the referee, not the exit status, decides
