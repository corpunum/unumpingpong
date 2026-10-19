#!/bin/sh
# Scripted persistent orchestrator: reads a message of cards on stdin and answers with
# JSON decisions, one per "CARD <table_id> <name> rally=N referee=PASS|FAIL done=..." line.
#
#   referee=PASS -> accept; for the table named ORCH_CHAIN_FROM also chain next = ORCH_NEXT
#   referee=FAIL -> return with note ORCH_NOTE
#   ORCH_LOG     file: every message received is appended
#   ORCH_MODE    garbage (no JSON) | crash (exit 1)
set -eu
in=$(cat)
if [ -n "${ORCH_LOG:-}" ]; then
  printf '%s\n=====\n' "$in" >> "$ORCH_LOG"
fi
case "${ORCH_MODE:-}" in
  garbage) echo "Looks fine to me."; exit 0 ;;
  crash) echo "orchestrator down" >&2; exit 1 ;;
esac
printf '%s\n' "$in" | grep '^CARD ' | {
  sep=''
  printf '{"decisions":['
  while read -r _ tid name _rally ref _done; do
    if [ "$ref" = "referee=PASS" ]; then
      if [ -n "${ORCH_CHAIN_FROM:-}" ] && [ "$name" = "$ORCH_CHAIN_FROM" ]; then
        printf '%s{"table":"%s","verdict":"accept","next":"%s"}' "$sep" "$tid" "${ORCH_NEXT:-next ball}"
      else
        printf '%s{"table":"%s","verdict":"accept"}' "$sep" "$tid"
      fi
    else
      printf '%s{"table":"%s","verdict":"return","note":"%s"}' "$sep" "$tid" "${ORCH_NOTE:-fix it}"
    fi
    sep=','
  done
  printf ']}\n'
}
