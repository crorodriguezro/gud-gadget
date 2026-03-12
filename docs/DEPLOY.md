# Deploying gud-drm to postmarketOS (OnePlus 6)

This guide covers building, deploying, and running the GUD gadget driver on a postmarketOS device.

For the current OnePlus target in this workspace, use:

- SSH target: `cristian@192.168.1.106`
- Hostname: `oneplus-enchilada`
- DRM node: `/dev/dri/card0`
- UDC: `a600000.usb`

Project helper scripts:

- `scripts/oneplus-status.sh`
- `scripts/deploy-oneplus.sh`
- `scripts/run-oneplus-gud.sh`

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

The host PC needs the GUD kernel driver (available in Linux 5.13+). When the OnePlus is connected and gud-drm is running:

```bash
# Check if GUD device is recognized
ls /sys/class/drm/ | grep gud

# The display should appear as a DRM device
# Check dmesg for GUD driver messages
dmesg | grep -i gud
```
