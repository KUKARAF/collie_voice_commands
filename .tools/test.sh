#!/usr/bin/env bash
# Run the Rust test suite against the host target (not Android) — fast, no emulator needed.
# Anything platform-specific belongs behind a cfg(target_os = "android") gate so it's exercised
# separately, not skipped silently here.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
source .tools/lib.sh

docker_run bash -c 'cd src-tauri && cargo test "$@"' _ "$@"
