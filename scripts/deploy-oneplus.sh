#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ONEPLUS_HOST="${ONEPLUS_HOST:-cristian@172.16.42.1}"
BINARY_PATH="${1:-${ROOT_DIR}/target/aarch64-unknown-linux-musl/release/gud-drm}"

if [[ ! -f "${BINARY_PATH}" ]]; then
    echo "Binary not found: ${BINARY_PATH}" >&2
    exit 1
fi

scp "${BINARY_PATH}" "${ONEPLUS_HOST}:~/gud-drm"
ssh -o BatchMode=yes "${ONEPLUS_HOST}" 'chmod +x ~/gud-drm && ls -l ~/gud-drm'
