#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Check every task in bench/open/tasks.json the way the benchmark builds it:
the parent commit (git archive) plus the fix's test must FAIL, and the same checkout plus the
fix's source changes (the real upstream fix, test files excluded) must PASS.

    python3 bench/open/verify.py [--repos-dir ~/.cache/upp-bench-repos]

Standard library only; needs git and each task's test runner (pytest for the Python set).
"""
import argparse, json, os, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ap = argparse.ArgumentParser()
ap.add_argument("--tasks", default=os.path.join(HERE, "tasks.json"))
ap.add_argument("--repos-dir", default=os.path.expanduser("~/.cache/upp-bench-repos"))
a = ap.parse_args()
spec = json.load(open(a.tasks))


def clone(url):
    d = os.path.join(a.repos_dir, url.rstrip("/").split("/")[-1])
    if not os.path.isdir(d):
        os.makedirs(a.repos_dir, exist_ok=True)
        subprocess.run(["git", "clone", "-q", "--filter=blob:none", url, d], check=True)
    return d


def run(cmd, cwd):
    try:
        return subprocess.run(cmd, shell=True, cwd=cwd, capture_output=True, text=True, timeout=300).returncode
    except subprocess.TimeoutExpired:
        return -9


bad = 0
for t in spec["tasks"]:
    repo = clone(t["url"])
    with tempfile.TemporaryDirectory() as d:
        arc = subprocess.run(["git", "-C", repo, "archive", t["base"]], capture_output=True, check=True)
        subprocess.run(["tar", "-x", "-C", d], input=arc.stdout, check=True)
        test = subprocess.run(["git", "-C", repo, "show", f"{t['commit']}:{t['test']}"], capture_output=True, check=True)
        os.makedirs(os.path.dirname(os.path.join(d, t["test"])), exist_ok=True)
        open(os.path.join(d, t["test"]), "wb").write(test.stdout)
        before = run(t["cmd"], d)
        # The real fix, source only: the diff parent..fix without test files.
        diff = subprocess.run(["git", "-C", repo, "diff", t["base"], t["commit"], "--", ".", f":!{os.path.dirname(t['test'])}"],
                              capture_output=True, check=True).stdout
        applied = subprocess.run(["git", "apply", "-"], cwd=d, input=diff, capture_output=True).returncode == 0
        after = run(t["cmd"], d) if applied else None
    ok = before != 0 and applied and after == 0
    bad += not ok
    print(f"{'ok ' if ok else 'BAD'} {t['id']:28} before={before} fix_applied={applied} after={after}")
print(f"{len(spec['tasks']) - bad}/{len(spec['tasks'])} tasks verified")
sys.exit(1 if bad else 0)
