#!/usr/bin/env bash
set -euo pipefail

ONEPLUS_HOST="${ONEPLUS_HOST:-cristian@172.16.42.1}"
DRM_CARD="${DRM_CARD:-/dev/dri/card0}"
RUST_LOG_LEVEL="${RUST_LOG_LEVEL:-debug}"
DOAS_PASSWORD="${DOAS_PASSWORD:-}"

REMOTE_CMD=$(cat <<EOF
set -e
doas sh -lc '
systemctl stop greetd
pkill -f gud-drm >/dev/null 2>&1 || true
nohup env RUST_LOG=${RUST_LOG_LEVEL} /home/cristian/gud-drm ${DRM_CARD} > /home/cristian/gud.log 2>&1 < /dev/null &
sleep 1
pgrep -af gud-drm || true
tail -n 40 /home/cristian/gud.log 2>/dev/null || true
'
EOF
)

if [[ -n "${DOAS_PASSWORD}" ]]; then
    printf '%s\n' "${DOAS_PASSWORD}" | ssh -tt -o BatchMode=yes "${ONEPLUS_HOST}" "${REMOTE_CMD}"
    exit $?
fi

ssh -tt -o BatchMode=yes "${ONEPLUS_HOST}" "${REMOTE_CMD}"
