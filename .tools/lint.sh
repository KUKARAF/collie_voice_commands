#!/usr/bin/env bash
# Matches what CI/reviewers should enforce: fmt check + clippy with warnings denied.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
source .tools/lib.sh

docker_run bash -c 'cargo fmt --check && cargo clippy --all-targets -- -D warnings'
