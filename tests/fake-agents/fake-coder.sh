#!/bin/sh
# Scripted stand-in for a coding agent, so tests and demos need no model.
# It reads the ball on stdin and gets the work right only from rally FAKE_PASS_ON on.
#
#   FAKE_FILE     file to write (relative to the table cwd)        default: answer.txt
#   FAKE_GOOD     content written from rally FAKE_PASS_ON on        default: 42
#   FAKE_BAD      content written before that                        default: 41
#   FAKE_PASS_ON  first rally that gets it right                     default: 2
#   FAKE_SLEEP    seconds to "think" per rally (fractions allowed)   default: 0
#   FAKE_LOG      directory: each ball received is saved as ball-<rally>.txt
#   FAKE_FAIL_ON  a rally on which the agent crashes (exit 3)
# The table sets UPP_RALLY, UPP_TABLE, UPP_SESSION and UPP_CWD.
set -eu
rally="${UPP_RALLY:-1}"
ball=$(cat)
if [ -n "${FAKE_LOG:-}" ]; then
  mkdir -p "$FAKE_LOG"
  printf '%s\n' "$ball" > "$FAKE_LOG/ball-$rally.txt"
fi
sleep "${FAKE_SLEEP:-0}"
if [ -n "${FAKE_FAIL_ON:-}" ] && [ "$rally" = "$FAKE_FAIL_ON" ]; then
  echo "fake agent crashed on rally $rally" >&2
  exit 3
fi
file="${FAKE_FILE:-answer.txt}"
mkdir -p "$(dirname "$file")"
if [ "$rally" -ge "${FAKE_PASS_ON:-2}" ]; then
  printf '%s\n' "${FAKE_GOOD:-42}" > "$file"
  echo "Rally $rally: fixed it, wrote $file. The referee output showed what was wrong."
else
  printf '%s\n' "${FAKE_BAD:-41}" > "$file"
  echo "Rally $rally: done, wrote $file."
fi
