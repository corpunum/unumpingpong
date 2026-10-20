#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# opencode in the podman sandbox: see agent-sandbox.sh.
exec "$(dirname "$0")/agent-sandbox.sh" opencode "$@"
