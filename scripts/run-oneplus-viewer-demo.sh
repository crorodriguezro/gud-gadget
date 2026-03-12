#!/usr/bin/env bash
set -euo pipefail

ONEPLUS_HOST="${ONEPLUS_HOST:-cristian@192.168.1.106}"
RUST_LOG_LEVEL="${RUST_LOG_LEVEL:-info}"
DOAS_PASSWORD="${DOAS_PASSWORD:-}"
VIEWER_UID="${VIEWER_UID:-10000}"
VIEWER_GID="${VIEWER_GID:-10000}"

REMOTE_CMD=$(cat <<EOF
set -e
doas sh -lc '
systemctl stop usb-moded.service >/dev/null 2>&1 || true
systemctl mask --runtime usb-moded.service >/dev/null 2>&1 || true
systemctl stop gud-viewerd.service >/dev/null 2>&1 || true
pkill -x gud-viewerd >/dev/null 2>&1 || true
systemd-run --unit gud-viewerd --collect --same-dir \
  --property=Restart=on-failure \
  --property=RestartSec=1s \
  --property=StartLimitIntervalSec=0 \
  --setenv=RUST_LOG=${RUST_LOG_LEVEL} \
  --setenv=GUD_VIEWER_UID=${VIEWER_UID} \
  --setenv=GUD_VIEWER_GID=${VIEWER_GID} \
  /home/cristian/gud-viewerd
sleep 1
systemctl status gud-viewerd.service --no-pager --full || true
journalctl -u gud-viewerd.service --no-pager -n 40 || true
'
printf '\nLaunch the viewer app in the phone session with:\n'
printf '  /home/cristian/gud-viewer-gtk\n'
EOF
)

if [[ -n "${DOAS_PASSWORD}" ]]; then
    python - "${ONEPLUS_HOST}" "${DOAS_PASSWORD}" "${REMOTE_CMD}" <<'PY'
import pexpect
import shlex
import sys

host, password, remote_cmd = sys.argv[1], sys.argv[2], sys.argv[3]
child = pexpect.spawn(
    f"ssh -tt -o BatchMode=yes {shlex.quote(host)} {shlex.quote(remote_cmd)}",
    encoding="utf-8",
    timeout=60,
)
index = child.expect([r"password:", pexpect.EOF, pexpect.TIMEOUT])
if index != 0:
    print(child.before, end="")
    raise SystemExit(1)
child.sendline(password)
child.expect(pexpect.EOF)
print(child.before, end="")
PY
    exit $?
fi

ssh -tt -o BatchMode=yes "${ONEPLUS_HOST}" "${REMOTE_CMD}"
