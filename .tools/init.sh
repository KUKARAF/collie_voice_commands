#!/usr/bin/env bash
# One-time (or after a Tauri version bump) project scaffold: `cargo tauri init` +
# `cargo tauri android init`. Safe to re-run — Tauri skips files that already exist.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
source .tools/lib.sh

docker_run bash -c '
    set -euo pipefail
    if [ ! -f Cargo.toml ]; then
        cargo tauri init --ci \
            --app-name collie-voice-commands \
            --window-title "Collie Voice Commands" \
            --frontend-dist . \
            --dev-url http://localhost:1420
    fi
    cargo tauri android init
'
