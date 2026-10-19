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
#   FAKE_CARD     when set, the answer carries a code block and a <card> tail block
#   FAKE_HANG_ON  a rally on which it hangs (sleeps 60 s) once; a marker file makes the
#                 retry of that rally (after a crash/resume) answer normally
# The table sets UPP_RALLY, UPP_TABLE, UPP_SESSION, UPP_CWD, UPP_DB, UPP_EFFORT.
set -eu
rally="${UPP_RALLY:-1}"
ball=$(cat)
if [ -n "${FAKE_LOG:-}" ]; then
  mkdir -p "$FAKE_LOG"
  printf '%s\n' "$ball" > "$FAKE_LOG/ball-$rally.txt"
  printf '%s\n' "${UPP_EFFORT:-}" > "$FAKE_LOG/effort-$rally.txt"
fi
if [ -n "${FAKE_HANG_ON:-}" ] && [ "$rally" = "$FAKE_HANG_ON" ] && [ ! -e .fake-hung ]; then
  touch .fake-hung
  sleep 60
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
  good=yes
else
  printf '%s\n' "${FAKE_BAD:-41}" > "$file"
  echo "Rally $rally: done, wrote $file."
  good=no
fi
if [ -n "${FAKE_CARD:-}" ]; then
  printf '```text\nSECRET-BODY-LINE %s\n```\n' "$rally"
  printf '<card>\ndone: yes\nsummary: wrote %s on rally %s\nevidence: checked the file (good=%s)\nopen_issues: none\nartifacts: %s\n</card>\n' "$file" "$rally" "$good" "$file"
fi
