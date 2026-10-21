#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Find benchmark tasks in a public git repository: real fix commits whose own test fails
before the fix and passes after it.

For each non-merge commit that changes both source and test files (and only a little source):
  1. rebuild the commit's PARENT with `git archive` (no history) and add the fix's test files;
  2. run the test command: it must FAIL with test failures (pytest exit 1, not a collection or
     import error: a missing new function is a feature request, not a bug);
  3. rebuild the FIX commit the same way: the same tests must PASS.
Every candidate that passes both checks is printed as one JSON line, ready for tasks.json.

    python3 bench/open/mine.py --repo /path/to/clone --url https://github.com/owner/name \\
        --tests 'tests/' --max-commits 300 --max-src-lines 40 > candidates.jsonl

Python projects only for now (`--cmd` defaults to pytest on the changed test files).
Standard library only; needs git and the project's test runner.
"""
import argparse, json, os, re, shutil, subprocess, sys, tempfile

ap = argparse.ArgumentParser()
ap.add_argument("--repo", required=True, help="local clone (full history)")
ap.add_argument("--url", required=True, help="public URL recorded in the task")
ap.add_argument("--tests", default="tests/", help="path prefix of test files")
ap.add_argument("--src-ext", default=".py")
ap.add_argument("--max-commits", type=int, default=300)
ap.add_argument("--max-src-lines", type=int, default=40, help="changed non-test lines at most")
ap.add_argument("--cmd", default="python3 -m pytest -q -x -p no:cacheprovider {tests}")
ap.add_argument("--timeout", type=int, default=180)
ap.add_argument("--want", type=int, default=12, help="stop after this many tasks")
a = ap.parse_args()

def git(*args):
    return subprocess.run(["git", "-C", a.repo, *args], capture_output=True, text=True).stdout

def build(commit, dest, tests_from=None, test_files=()):
    os.makedirs(dest)
    arch = subprocess.run(["git", "-C", a.repo, "archive", commit], capture_output=True)
    subprocess.run(["tar", "-x", "-C", dest], input=arch.stdout, check=True)
    for t in test_files:
        blob = subprocess.run(["git", "-C", a.repo, "show", f"{tests_from}:{t}"], capture_output=True)
        p = os.path.join(dest, t)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        open(p, "wb").write(blob.stdout)

def run(dest, test_files):
    cmd = a.cmd.format(tests=" ".join(test_files))
    try:
        r = subprocess.run(cmd, shell=True, cwd=dest, capture_output=True, text=True, timeout=a.timeout,
                           env={**os.environ, "PYTHONPATH": ":".join(p for p in (os.path.join(dest, "src"), dest)),
                                "PYTHONDONTWRITEBYTECODE": "1"})
        return r.returncode, (r.stdout + r.stderr)[-1500:]
    except subprocess.TimeoutExpired:
        return -9, "timeout"

found = 0
for line in git("log", "--no-merges", f"-{a.max_commits}", "--format=%H%x09%s").splitlines():
    sha, subject = line.split("\t", 1)
    stat = git("show", "--numstat", "--format=", sha).splitlines()
    tests, src_lines, src_files = [], 0, []
    for s in stat:
        parts = s.split("\t")
        if len(parts) != 3 or parts[0] == "-":
            continue
        add, rm, path = int(parts[0]), int(parts[1]), parts[2]
        if path.startswith(a.tests) and path.endswith(".py"):
            tests.append(path)
        elif path.endswith(a.src_ext):
            src_lines += add + rm
            src_files.append(path)
    if not tests or not src_files or src_lines > a.max_src_lines:
        continue
    if not git("rev-parse", "--verify", "-q", f"{sha}^"):
        continue
    with tempfile.TemporaryDirectory() as tmp:
        before, after = os.path.join(tmp, "before"), os.path.join(tmp, "after")
        build(f"{sha}^", before, sha, tests)
        rc_b, out_b = run(before, tests)
        if rc_b != 1 or re.search(r"(ImportError|ModuleNotFoundError|AttributeError: module|errors? during collection)", out_b):
            continue
        build(sha, after)
        rc_a, _ = run(after, tests)
        if rc_a != 0:
            continue
    found += 1
    print(json.dumps({"repo": a.url, "fix": sha, "parent": git("rev-parse", f"{sha}^").strip(),
                      "issue": subject, "tests": tests, "src_files": src_files, "src_lines": src_lines,
                      "cmd": a.cmd.format(tests=" ".join(tests))}), flush=True)
    if found >= a.want:
        break
print(f"# {found} tasks", file=sys.stderr)
