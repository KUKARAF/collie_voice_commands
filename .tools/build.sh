#!/usr/bin/env bash
# Build the Android APK. Pass --release for a release build; defaults to debug.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
source .tools/lib.sh

docker_run cargo tauri android build "$@"
