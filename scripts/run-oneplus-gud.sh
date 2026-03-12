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
systemctl stop gud-userspace.service >/dev/null 2>&1 || true
pkill -f gud-drm >/dev/null 2>&1 || true
systemd-run --unit gud-userspace --collect --same-dir \
  --property=Restart=on-failure \
  --property=RestartSec=1s \
  --property=StartLimitIntervalSec=0 \
  --setenv=RUST_LOG=${RUST_LOG_LEVEL} \
  /home/cristian/gud-drm ${DRM_CARD}
sleep 1
systemctl status gud-userspace.service --no-pager --full || true
journalctl -u gud-userspace.service --no-pager -n 40 || true
'
EOF
)

if [[ -n "${DOAS_PASSWORD}" ]]; then
    printf '%s\n' "${DOAS_PASSWORD}" | ssh -tt -o BatchMode=yes "${ONEPLUS_HOST}" "${REMOTE_CMD}"
    exit $?
fi

ssh -tt -o BatchMode=yes "${ONEPLUS_HOST}" "${REMOTE_CMD}"
