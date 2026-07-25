# Deploying gud-drm

This guide covers building, deploying, and running the GUD gadget driver.

Project helper scripts:

- `scripts/oneplus-status.sh`
- `scripts/deploy-oneplus.sh`
- `scripts/run-oneplus-gud.sh`

## Targets

| Device | SSH target | Hostname | DRM node | UDC |
|--------|-----------|----------|----------|-----|
| OnePlus 6 | `phablet@192.168.1.120` | `ubuntu-phablet` | `/dev/dri/card0` | `a600000.usb` |
| RPi Zero 2 W | `cristian@192.168.1.110` | `raspberrypi` | `/dev/dri/card0` | `3f980000.usb` |

For the Pi test target above, use SSH username `cristian` and password
`cristian`.

For the OnePlus test target above, use SSH username `phablet` and password
`1026`.

## Prerequisites

### On the build machine (Fedora)

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install cross-compilation tools
sudo dnf install gcc-aarch64-linux-gnu sysroot-aarch64-fc43-glibc

# Install cross for Docker-based cross-compilation
cargo install cross --git https://github.com/cross-rs/cross

# Ensure Docker is running
sudo systemctl start docker
```

### On the OnePlus device

- postmarketOS installed with SSH access
- USB cable connected to host PC for GUD functionality

## Build

### Cross-compile for aarch64 with musl (for postmarketOS/Alpine)

```bash
# Clone and enter the repository
git clone https://github.com/your-repo/gud-gadget.git
cd gud-gadget

# Checkout desired branch (main or dma-buf-overhaul)
git checkout main
# or
git checkout dma-buf-overhaul

# Build using cross (uses Docker for proper cross-compilation)
cross build --release --target aarch64-unknown-linux-musl
```

The binary will be at: `target/aarch64-unknown-linux-musl/release/gud-drm`

## Deploy to Device

```bash
# Copy binary to device
scp target/aarch64-unknown-linux-musl/release/gud-drm cristian@192.168.1.106:~/

# Make it executable
ssh cristian@192.168.1.106 "chmod +x ~/gud-drm"

# Or use the project helper
./scripts/deploy-oneplus.sh
```

## Run on Device

### 1. Stop the UI service

postmarketOS uses `greetd` which runs `phoc` (Wayland compositor). This holds the DRM device.
On newer images, `usb-moded` may also be active and will fight the userspace
gadget by reprogramming USB mode back to NCM/mass-storage. Stop it for the GUD
session.

```bash
# SSH into the device
ssh -tt cristian@192.168.1.106

# Stop the greeter/display manager
doas systemctl stop greetd

# Stop the phone USB-mode manager for this boot/session
doas systemctl stop usb-moded.service || true
doas systemctl mask --runtime usb-moded.service || true

# Stop the local tty login from reclaiming/blanking the panel
doas systemctl stop getty@tty1.service || true
doas systemctl mask --runtime getty@tty1.service || true
doas pkill agetty || true
doas sh -lc 'echo 0 > /sys/class/vtconsole/vtcon1/bind || true'
doas sh -lc 'echo 0 > /sys/class/graphics/fb0/blank || true'
```

### 2. Verify DRM device is free

```bash
# Should return nothing if DRM is free
doas fuser /dev/dri/card0

# If processes are still using it, check which ones
ps aux | grep -E 'phoc|phosh|weston'
```

### 3. Verify USB connection

The OnePlus must be connected to a host PC via USB (data cable, not charge-only).

```bash
# Check UDC state - should show "configured" or "not attached"
cat /sys/class/udc/a600000.usb/state
```

- `configured` - USB connected to host
- `not attached` - USB not connected or host not ready

### 4. Run gud-drm

**Important:** On postmarketOS with `doas`, environment variables are not passed through by default. Use `doas env` to pass `RUST_LOG`.

The current OnePlus setup requires `doas` authentication. You can either run
the commands manually over an interactive SSH session, or provide
`DOAS_PASSWORD` to the helper script so it can authenticate once and then stop
`greetd` and `usb-moded` before launching `gud-drm`.

```bash
# Run in foreground (for testing, see logs directly)
doas env RUST_LOG=debug ~/gud-drm /dev/dri/card0

# Run in background with logging to file
nohup doas env RUST_LOG=debug ~/gud-drm /dev/dri/card0 > ~/gud.log 2>&1 &

# Check if running
ps aux | grep gud-drm

# View logs
tail -f ~/gud.log

# Check kernel messages for USB/gadget issues
doas dmesg | grep -E 'usb|gadget|dwc3|ffs'

# Or use the project helper from the repo root on the build machine
./scripts/run-oneplus-gud.sh

# Or let the helper authenticate once for you
DOAS_PASSWORD=123 ./scripts/run-oneplus-gud.sh
```

#### Log Levels

Set `RUST_LOG` to control verbosity:
- `RUST_LOG=error` - Only errors
- `RUST_LOG=warn` - Warnings and errors
- `RUST_LOG=info` - General information (recommended for normal use)
- `RUST_LOG=debug` - Detailed debug info (recommended for troubleshooting)
- `RUST_LOG=trace` - Very verbose (per-packet details)

#### Log File Location

When running in background, logs are written to `~/gud.log` (home directory) or wherever you redirect output.

#### Framebuffer Dumps For Debugging

You can make `gud-drm` write the current framebuffer to a PPM image file after
the initial test pattern and after each frame update.

```bash
doas env RUST_LOG=debug \
  GUD_DUMP_FB_PATH=/home/cristian/gud-framebuffer.ppm \
  ~/gud-drm /dev/dri/card0
```

Useful when the panel image looks wrong but the USB gadget is otherwise alive:

```bash
# Copy the dump back to the host for inspection
# On this phone, plain scp can stall. This gzip-over-ssh path is reliable.
ssh cristian@192.168.1.115 'gzip -1 -c /home/cristian/gud-framebuffer.ppm' \
  > /tmp/gud-framebuffer.ppm.gz
gunzip -f /tmp/gud-framebuffer.ppm.gz
```

This shows what `gud-drm` thinks it rendered, which helps separate:
- protocol / buffer bugs
- framebuffer pitch or geometry bugs
- panel / compositor / scanout issues

#### Alternative: Run with nohup in a script

Create a startup script `~/start-gud.sh`:
```bash
#!/bin/sh
nohup doas env RUST_LOG=debug ~/gud-drm /dev/dri/card0 > ~/gud.log 2>&1 &
```

Then run:
```bash
chmod +x ~/start-gud.sh
./start-gud.sh
```

## Troubleshooting

### DRM device busy / Permission denied

```
Could not set CRTC: Os { code: 13, kind: PermissionDenied }
```

Another process is using the DRM device. Stop the display manager:

```bash
doas systemctl stop greetd
```

Check what's using it:

```bash
doas fuser -v /dev/dri/card0
```

### USB not attached

```
cat /sys/class/udc/a600000.usb/state
not attached
```

- Ensure USB cable is a **data cable** (not charge-only)
- Ensure OnePlus is connected to a host PC
- Try a different USB port or cable

### Binary won't execute / Command not found

If you get "command not found" or linker errors, the binary was compiled for the wrong libc.

postmarketOS uses **musl libc**, so build with:

```bash
cross build --release --target aarch64-unknown-linux-musl
```

NOT `aarch64-unknown-linux-gnu` (glibc).

### Kernel panic / device reboots

The DMA-BUF overhaul branch may have issues on some devices. Try the main branch:

```bash
git checkout main
cross build --release --target aarch64-unknown-linux-musl
```

## Restore UI

To restore the graphical interface:

```bash
doas systemctl start greetd
```

## On the Host PC

The host PC needs the GUD kernel driver (available in Linux 5.13+). When the gadget device is connected and gud-drm is running:

```bash
# Check if GUD device is recognized
lsusb | grep 1d50:614d
ls /sys/class/drm/ | grep USB

# The display should appear as a DRM device
# Check dmesg for GUD driver messages
dmesg | grep -i gud
```

# Raspberry Pi Zero 2 W Setup

## Hardware Notes

The RPi Zero 2 W has a single micro-USB port (OTG) and a mini-HDMI port.

- **Micro-USB port**: OTG-capable, used for USB gadget mode. Must be connected to the host PC with a **data cable** (not charge-only).
- **Mini-HDMI port**: Video output only, used to see the "WAITING" screen on the device.

**Cable compatibility**: Not all micro-USB data cables work for gadget mode. Out of
three cables tested (two short black, one long white), only the long white cable
worked. If the UDC state stays `not attached`, try a different cable — even cables
that work for file transfer may lack the OTG pin wiring needed for gadget mode.

The RPi Zero 2 W runs Debian with kernel `6.12.47+rpt-rpi-v8`. The kernel has
FunctionFS support built-in (`CONFIG_USB_CONFIGFS_F_FS=y`, `CONFIG_USB_F_FS=m`).

## First-Time Setup

### 1. Connect a monitor and keyboard

Plug a monitor into the mini-HDMI port and a keyboard into a USB hub on the
micro-USB port (or connect via WiFi/SSH if already configured).

### 2. Deploy the binary

```bash
# From the build machine
scp ~/gud-drm cristian@192.168.1.110:~/
ssh cristian@192.168.1.110 "chmod +x ~/gud-drm"
```

### 3. Stop the local display and start gud-drm manually

```bash
ssh cristian@192.168.1.110

# Unbind the virtual console to free the DRM device
sudo sh -c 'echo 0 > /sys/class/vtconsole/vtcon1/bind'
sudo sh -c 'echo 0 > /sys/class/graphics/fb0/blank'

# Start gud-drm
sudo ~/gud-drm /dev/dri/card0
```

The screen will show a "WAITING" penguin. At this point the USB gadget is set up
but the display won't show the host desktop until the USB data cable is connected
**and** the host enumerates the device.

### 4. Enable auto-start on boot

The RPi Zero 2 W starts `gud-drm` automatically on boot via a systemd service.
To set this up:

```bash
# Create the service file
sudo tee /etc/systemd/system/gud-userspace.service << 'EOF'
[Unit]
Description=GUD Userspace Display Driver
After=multi-user.target

[Service]
Type=simple
# Keep fbcon from restoring its stale vc4 state when gud-drm exits.
ExecStartPre=/bin/sh -c 'echo 0 > /sys/class/vtconsole/vtcon1/bind'
ExecStart=/home/cristian/gud-drm /dev/dri/card0
# XDISP-P0.1 containment: never rebind automatically after a USB failure.
Restart=no

[Install]
WantedBy=multi-user.target
EOF

# Enable the service
sudo systemctl enable gud-userspace.service
```

The tracked containment drop-in is
`../systemd/gud-userspace.service.d/10-xdisp-p0.1-containment.conf`. Installing
it requires `systemctl daemon-reload`, but do not restart an already-running
service merely to load the policy. It sets `Restart=no` and `SendSIGKILL=no`.
The latter prevents systemd from forcing teardown if an in-flight or poisoned
Step 5 process intentionally ignores `SIGTERM`.

The Step 5 read-size drop-in is
`../systemd/gud-userspace.service.d/20-xdisp-p0.1-ffs-read-size.conf`:

```ini
[Service]
Environment=GUD_FFS_READ_SIZE=16384
Environment=RUST_LOG=debug
```

The value is a receive ceiling, not a padded request size. `gud-drm` requests
exactly the remaining payload when it is smaller. The staged 16,384-byte
setting receives a normal 64,000-byte tile as
`[16384, 16384, 16384, 14848]`, replacing 125 separate 512-byte reads while
limiting contiguous-allocation pressure. Valid values are 4,096 through 65,536
bytes in 512-byte increments. Invalid values fail before DRM or UDC setup; the
service never silently falls back to the old 512-byte loop. The 65,536-byte
ceiling is reserved for an explicit later A/B test and is not the first
hardware setting. The debug filter is pinned so every read start/completion is
retained in the service journal.

Install this drop-in only together with the matching Step 5 binary. Reloading
systemd is safe, but activation requires the separately controlled
stop/start-and-payload procedure in
`XDISP-P0.1-FUNCTIONFS-REBIND-TEST.md`.

### XDISP-P0.1 test-only descriptor controls

The diagnostic binary accepts two explicitly test-only environment variables:

```text
GUD_TEST_COMPRESSION=lz4|none
GUD_TEST_MAX_BUFFER_SIZE=<positive u32>
```

When absent, behavior is unchanged: the gadget advertises LZ4 and its natural
maximum buffer size. `GUD_TEST_COMPRESSION` accepts exactly `lz4` or `none`;
the maximum-buffer override must be nonzero and no larger than the natural
maximum. Invalid values fail before DRM or UDC setup. A startup warning makes
any active override visible in the journal.

The reproducible Gate A, Gate B, and Gate C examples live under
`systemd/test-only/`. They are not normal service configuration and must never
be installed together. Changing either descriptor value requires a fresh Pi
boot and USB enumeration; reusing a host's cached descriptor is not valid
evidence. Gate A retains LZ4 with a 64,000-byte maximum. Gate B keeps the same
maximum and advertises no compression; it reproduced the terminal DWC2
residual failure at an actual 61,440-byte transfer and must not be reinstalled
for an unchanged test. Gate C advertises no compression with a 15,360-byte
maximum. That value is high-speed-packet aligned and forms complete RGB565
rows at both 1280 and 1920 pixels wide, making it the first no-kernel-build
transfer-length isolation gate.

The lifecycle-repair build enables `ctrlc` termination handling. Before every
blocking FunctionFS receive, Step 5 atomically changes the session from
`Idle` to `InFlight`. `SIGTERM` may claim and unbind the UDC only from `Idle`;
an in-flight or poisoned session ignores it. A successful receive under the
conservative one-second safety threshold returns to `Idle`; a returned error
or late completion becomes permanently `Poisoned` and refuses further
USB/control work. A hung read remains `InFlight`. Decompression and optional
dumps run after the session returns to `Idle`. For a proven-idle stop, teardown
removes the gadget, closes the remaining endpoint owners, and only then
releases DRM. Do not use `SIGKILL` for routine shutdown. For an
in-flight/poisoned instance, do not stop, restart, reboot, or shut down; use a
physical power cycle, hardware reset, or watchdog reset and collect
previous-boot evidence.

Note: The service file may have warnings about `StartLimitIntervalSec` — this key
is not recognized in the `[Service]` section (it belongs in `[Unit]`) but it is
harmless.

### 5. Reboot with the USB cable connected

After rebooting with the micro-USB data cable connected to the host PC, the
device will automatically:

1. Start `gud-drm` on boot
2. Enumerate as a USB GUD display (`1d50:614d`)
3. Appear as a new DRM output on the host (`card*-USB-*`)
4. Display the host's extended/mirrored desktop

No manual SSH intervention is needed after the initial setup.

## Troubleshooting (RPi Zero 2 W)

### Display shows "WAITING" but host doesn't see it

- Check the UDC state: `cat /sys/class/udc/3f980000.usb/state`
  - `configured` = USB connected and enumerated
  - `not attached` = no USB data connection (try a different cable or port)
- Ensure the micro-USB **data** cable is connected to the host PC
- Verify on the host: `lsusb | grep 1d50:614d`

### UDC shows "UDC had already started"

A previous `gud-drm` instance or the systemd service already bound the gadget.
Kill the duplicate:

```bash
ps aux | grep gud-drm | grep -v grep
sudo kill <PID>
```

### SSH becomes unreachable when gud-drm is running

`usb_gadget::remove_all()` is called at startup, which tears down all existing
USB gadgets. On the RPi Zero 2 W this can disrupt the WiFi adapter if it shares
the USB bus. Connect via the mini-HDMI console. While `XDISP-P0.1` is blocked,
the service deliberately does not restart `gud-drm` automatically.

### Restore local display

```bash
sudo systemctl stop gud-userspace.service
sudo sh -c 'echo 1 > /sys/class/vtconsole/vtcon1/bind'
```
