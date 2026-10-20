#!/bin/bash
# Validate candidate fix commits: at fix^ with the fix's test file, the test must FAIL;
# at the fix commit it must PASS. Prints "ok <sha> <test> <src>" for usable tasks.
# usage: select.sh <repo> <node_modules> <sha>...
repo=$1; nm=$2; shift 2
for h in "$@"; do
  t=$(git -C "$repo" show --name-only --format= $h | grep '^tests/unit/.*\.test\.mjs$')
  s=$(git -C "$repo" show --name-only --format= $h | grep '^src/.*\.mjs$')
  w=$(mktemp -d /tmp/upp-sel-XXXX)
  git -C "$repo" worktree add -q --detach "$w" "$h^" 2>/dev/null
  ln -s "$nm" "$w/node_modules"
  git -C "$repo" show "$h:$t" > "$w/$t"
  (cd "$w" && timeout 180 npx vitest run "$t" >/dev/null 2>&1); before=$?
  git -C "$repo" show "$h:$s" > "$w/$s"
  (cd "$w" && timeout 180 npx vitest run "$t" >/dev/null 2>&1); after=$?
  [ $before -ne 0 ] && [ $after -eq 0 ] && echo "ok $h $t $s" || echo "skip $h before=$before after=$after"
  rm -f "$w/node_modules"; git -C "$repo" worktree remove --force "$w"
done
