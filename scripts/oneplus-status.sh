#!/usr/bin/env bash
set -euo pipefail

ONEPLUS_HOST="${ONEPLUS_HOST:-cristian@172.16.42.1}"

ssh -o BatchMode=yes "${ONEPLUS_HOST}" '
set -e
echo "hostname: $(hostname)"
echo "kernel: $(uname -a)"
echo
echo "DRM nodes:"
ls /dev/dri
echo
echo "UDC state:"
cat /sys/class/udc/a600000.usb/state
echo
echo "configfs gadgets:"
ls /sys/kernel/config/usb_gadget
echo
echo "gud-drm process:"
pgrep -af gud-drm || true
echo
echo "artifacts:"
ls -l ~/gud-drm ~/gud.log 2>/dev/null || true
'
