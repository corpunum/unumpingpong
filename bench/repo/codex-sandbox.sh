#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# codex in the podman sandbox: see agent-sandbox.sh.
exec "$(dirname "$0")/agent-sandbox.sh" codex "$@"
