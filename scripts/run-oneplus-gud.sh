#!/usr/bin/env bash
set -euo pipefail

ONEPLUS_HOST="${ONEPLUS_HOST:-cristian@192.168.1.106}"
DRM_CARD="${DRM_CARD:-/dev/dri/card0}"
RUST_LOG_LEVEL="${RUST_LOG_LEVEL:-debug}"
DOAS_PASSWORD="${DOAS_PASSWORD:-}"

REMOTE_CMD=$(cat <<EOF
set -e
doas sh -lc '
systemctl stop greetd
systemctl stop usb-moded.service >/dev/null 2>&1 || true
systemctl mask --runtime usb-moded.service >/dev/null 2>&1 || true
systemctl stop getty@tty1.service >/dev/null 2>&1 || true
systemctl mask --runtime getty@tty1.service >/dev/null 2>&1 || true
pkill agetty >/dev/null 2>&1 || true
echo 0 > /sys/class/vtconsole/vtcon1/bind 2>/dev/null || true
echo 0 > /sys/class/graphics/fb0/blank 2>/dev/null || true
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
