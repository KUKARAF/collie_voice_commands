#!/usr/bin/env bash
# Build (or rebuild after Dockerfile.build changes) the toolchain image every other .tools/
# script runs against. Run this once up front, and again whenever .tools/Dockerfile.build changes.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

docker build --load -f .tools/Dockerfile.build -t collie-voice-toolchain .
