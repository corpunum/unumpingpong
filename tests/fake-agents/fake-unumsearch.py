#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Stand-in for `unumsearch search -F -m N QUERY PATH` (JSON output): ten hits for any query
except "nothing", each in PATH/src/f<i>.rs, with a 300-character line; argv goes to
$FAKE_SEARCH_LOG when set. `unumsearch mcp` is not faked (the players' MCP wiring is checked
by argv only)."""
import json, os, sys

a = sys.argv[1:]
if os.environ.get("FAKE_SEARCH_LOG"):
    with open(os.environ["FAKE_SEARCH_LOG"], "a") as f:
        f.write(json.dumps(a) + "\n")
if not a or a[0] != "search":
    print(json.dumps({"ok": False, "error": "unsupported"}))
    sys.exit(2)
query, root = a[-2], a[-1]
hits = [] if query == "nothing" else [
    {"path": os.path.join(root, "src", f"f{i}.rs"), "line": i + 1, "text": f"  fn {query}_{i}() {{ {'x' * 300} }}"}
    for i in range(10)]
print(json.dumps({"ok": True, "result": {"backend": "fake", "fresh": True, "matches": hits[: int(a[a.index('-m') + 1])],
                                         "truncated": len(hits) > int(a[a.index('-m') + 1])}}))
