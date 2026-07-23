# Shared by every .tools/*.sh script. Not meant to be run directly.
set -euo pipefail

IMAGE="collie-voice-toolchain"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Cache Cargo/Gradle downloads across runs in named volumes instead of re-fetching every
# container invocation — first run is slow, everything after is fast. Gradle's wrapper
# re-downloads its ~120MB distribution zip on every run without this (container HOME is /root,
# ephemeral) — that repeated large download was the actual cause of "build failed" flakiness
# that looked like network issues but was really just bad luck hitting it every single build.
CACHE_VOLUME="collie-voice-toolchain-cache"
GRADLE_CACHE_VOLUME="collie-voice-toolchain-gradle-cache"

docker_run() {
    # :Z relabels the bind mount for this container's SELinux/userns context — without it, writes
    # from inside the (rootless podman-backed) container 403 on files owned by the host user.
    docker run --rm \
        -v "${REPO_ROOT}:/workspace:Z" \
        -v "${CACHE_VOLUME}:/usr/local/cargo/registry" \
        -v "${GRADLE_CACHE_VOLUME}:/root/.gradle" \
        -w /workspace \
        "${IMAGE}" \
        "$@"
}
