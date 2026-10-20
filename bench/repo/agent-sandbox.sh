#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Run an agent CLI (opencode, codex) inside a rootless podman container that can write ONLY
# the current directory. Neither opencode's permissions nor Codex's own sandbox (bubblewrap,
# blocked where unprivileged user namespaces are restricted) could fence a full-permission
# agent here (10-20: an agent left its task folder and checked out a commit into $HOME).
#
# Sees: the current directory (rw), node_modules symlink targets below it (ro), /usr (ro: node,
# git, python), CA certificates (ro), and a private throwaway HOME holding a copy of that tool's
# own config/auth only. Nothing else of the host.
# Usage: agent-sandbox.sh <opencode|codex> <the tool's usual arguments...>
# (The tool's whole binary directory is mounted: codex runs helper binaries that sit next to it.)
set -eu
TOOL=$1; shift
CWD=$(pwd -P)
# The writable area: this directory, or UPP_SANDBOX_ROOT (a parent) when one long-lived process
# serves several task folders in turn (shared sessions).
ROOT=${UPP_SANDBOX_ROOT:-$CWD}
SB=${UPP_SANDBOX_HOME:-${UPP_OC_SANDBOX_HOME:-/tmp/upp-sandbox-$TOOL}}
mkdir -p "$SB"; chmod 700 "$SB"
case "$TOOL" in
  opencode)
    BIN=$(readlink -f "$HOME/.local/bin/opencode")
    if [ ! -e "$SB/.config/opencode/opencode.jsonc" ]; then
      mkdir -p "$SB/.config" "$SB/.local/share/opencode"
      cp -a "$HOME/.config/opencode" "$SB/.config/"   # config + installed provider packages
    fi ;;
  codex)
    BIN=$(readlink -f "$HOME/.local/bin/codex")
    if [ ! -e "$SB/.codex/auth.json" ]; then
      mkdir -p "$SB/.codex"; chmod 700 "$SB/.codex"
      cp -p "$HOME/.codex/auth.json" "$SB/.codex/"     # credentials stay inside this 0700 home
      cp -p "$HOME/.codex/config.toml" "$SB/.codex/" 2>/dev/null || true
    fi ;;
  *) echo "agent-sandbox: unsupported tool $TOOL" >&2; exit 2 ;;
esac
# node_modules symlinks in this folder or one level down (a solo run sees all task folders).
NM=$(find "$ROOT" -maxdepth 3 -name node_modules -type l -exec readlink -f {} \; 2>/dev/null | sort -u | head -1)
# Label: killing the `podman run` client does not stop the container, so the caller stops
# leftovers by label (UPP_SANDBOX_LABEL, e.g. the bench run folder).
set -- --rm -i --userns=keep-id --network=host --label "upp-sandbox=${UPP_SANDBOX_LABEL:-default}" \
  -v /usr:/usr:ro -v /etc/ssl:/etc/ssl:ro -v /etc/ca-certificates:/etc/ca-certificates:ro \
  -v "$(dirname "$BIN"):/opt/agentbin:ro" \
  -v "$SB:/home/sandbox" -e HOME=/home/sandbox -e CODEX_HOME=/home/sandbox/.codex \
  -v "$ROOT:$ROOT" -w "$CWD" -e PWD="$CWD" \
  ${NM:+-v "$NM:$NM:ro"} \
  -e NVIDIA_API_KEY \
  docker.io/library/ubuntu:24.04 "/opt/agentbin/$(basename "$BIN")" "$@"
exec podman run "$@"
