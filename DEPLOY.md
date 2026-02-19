# Deploying gud-drm to postmarketOS (OnePlus 6)

This guide covers building, deploying, and running the GUD gadget driver on a postmarketOS device.

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
# Copy binary to device (adjust IP as needed)
scp target/aarch64-unknown-linux-musl/release/gud-drm user@192.168.1.115:~/

# Make it executable
ssh user@192.168.1.115 "chmod +x ~/gud-drm"
```

## Run on Device

### 1. Stop the UI service

postmarketOS uses `greetd` which runs `phoc` (Wayland compositor). This holds the DRM device.

```bash
# SSH into the device
ssh user@192.168.1.115

# Stop the greeter/display manager
doas systemctl stop greetd
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

```bash
# Run in foreground (for testing)
RUST_LOG=debug doas ~/gud-drm /dev/dri/card0

# Or run in background with logging
RUST_LOG=info doas nohup ~/gud-drm /dev/dri/card0 > /tmp/gud-drm.log 2>&1 &

# Check logs
tail -f /tmp/gud-drm.log

# Check kernel messages for USB/gadget issues
doas dmesg -w | grep -E 'usb|gadget|dwc3|ffs'
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
