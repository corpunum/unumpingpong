#!/bin/sh
# Answers with the first line of the ball it got, prefixed by the table name.
# Used for chaining tests: the downstream ball must contain the upstream answer.
set -eu
ball=$(cat)
sleep "${FAKE_SLEEP:-0}"
printf '%s: %s\n' "${UPP_TABLE:-?}" "$(printf '%s\n' "$ball" | head -n 1)"
