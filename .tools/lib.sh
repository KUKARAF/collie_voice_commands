# Shared by every .tools/*.sh script. Not meant to be run directly.
set -euo pipefail

IMAGE="collie-voice-toolchain"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Cache Cargo/Gradle/Android downloads across runs in a named volume instead of re-fetching
# every container invocation — first run is slow, everything after is fast.
CACHE_VOLUME="collie-voice-toolchain-cache"

docker_run() {
    # :Z relabels the bind mount for this container's SELinux/userns context — without it, writes
    # from inside the (rootless podman-backed) container 403 on files owned by the host user.
    docker run --rm \
        -v "${REPO_ROOT}:/workspace:Z" \
        -v "${CACHE_VOLUME}:/usr/local/cargo/registry" \
        -w /workspace \
        "${IMAGE}" \
        "$@"
}
