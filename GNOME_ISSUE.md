# GNOME/Mutter Wayland Multi-GPU Issue

## Summary

The GUD gadget driver works correctly on **KDE Plasma** but not on **GNOME on Wayland**. This is a known limitation of Mutter (GNOME's compositor) when handling multiple DRM devices.

## Symptoms

| Desktop | Behavior |
|---------|----------|
| KDE Plasma (Wayland) | ✅ GUD display appears in Display Settings, works automatically |
| GNOME (Wayland) | ❌ GUD display detected by Settings but cannot be enabled |
| GNOME (Xorg) | ❓ Not tested - Xorg session not available |

## Technical Details

### Device Detection

On both KDE and GNOME, the GUD device is properly registered:

```bash
$ ls /sys/class/drm/
card0           # GUD device
card0-USB-1     # GUD connector
card1           # AMD GPU
card1-HDMI-A-1  # Primary display
...
```

### xrandr Output Comparison

**KDE (working):**
```
Screen 0: minimum 16 x 16, current 9104 x 2880, maximum 32767 x 32767
eDP-1 connected primary 3024x1890+0+588
   ...
USB-1 connected 6080x2880+3024+0 left
   2880x6080     59.99*+
   ...
```

**GNOME (not working):**
```
Screen 0: minimum 16 x 16, current 1920 x 1080, maximum 32767 x 32767
HDMI-1 connected primary 1920x1080+0+0
   ...
   # USB-1 NOT LISTED
```

### DRM Providers

Both show 0 providers:
```
$ xrandr --listproviders
Providers: number : 0
```

### Kernel Messages

Both show GUD device initialized:
```
[drm] Initialized gud 1.0.0 for 1-1:1.0 on minor 0
gud 1-1:1.0: [drm] fb1: guddrmfb frame buffer device
```

## Root Cause

**Mutter on Wayland doesn't support rendering to multiple DRM devices.**

KDE's KWin compositor has built-in support for multi-GPU setups and automatically detects and renders to secondary DRM devices. Mutter on Wayland only renders to the primary GPU and doesn't expose secondary DRM devices through xrandr or the Display Settings UI.

This is a known limitation documented in several issues:
- [Mutter #1562](https://gitlab.gnome.org/GNOME/mutter/-/issues/1562) - Multi-GPU support
- [Mutter #1515](https://gitlab.gnome.org/GNOME/mutter/-/issues/1515) - PRIME render offload

## Gadget Driver Behavior

The gadget driver works correctly on both desktops. The difference is in what the host sends:

| Host | Compressed Size | Content |
|------|----------------|---------|
| KDE | ~577 KB | Actual desktop content |
| GNOME | ~19 KB | Black screen (no content rendered) |

From OnePlus logs:

**KDE:**
```
received set buffer: SetBuffer { ..., compressed_length: 577537 }
read 577537 bytes in 1129 packets
Framebuffer flushed
```

**GNOME:**
```
received set buffer: SetBuffer { ..., compressed_length: 19323 }
read 19323 bytes in 38 packets
Framebuffer flushed
Received event: Suspend
```

## Workarounds

### Option 1: Use GNOME on Xorg (if available)

1. Log out of GNOME
2. At the login screen, click the gear icon
3. Select "GNOME on Xorg"
4. Log in

Note: Some distributions don't include Xorg sessions by default.

### Option 2: Use KDE Plasma

KDE Plasma's KWin compositor properly supports multiple DRM devices.

### Option 3: Use Weston (reference Wayland compositor)

```bash
# Install weston
sudo dnf install weston  # Fedora
sudo apt install weston  # Debian/Ubuntu

# Run weston with both DRM devices
weston --device=/dev/dri/card0,/dev/dri/card1
```

### Option 4: Use Sway (i3-compatible Wayland compositor)

```bash
# Install sway
sudo dnf install sway  # Fedora
sudo apt install sway  # Debian/Ubuntu

# Run sway
sway
```

### Option 5: Use xf86-video-dummy + xrandr (advanced)

Create a custom Xorg configuration to manually configure the GUD device. This requires Xorg session.

## Future Outlook

Mutter multi-GPU support is being actively worked on. Check these resources for updates:

- [Mutter Merge Requests](https://gitlab.gnome.org/GNOME/mutter/-/merge_requests)
- [GNOME Shell Issues](https://gitlab.gnome.org/GNOME/gnome-shell/-/issues)

## Verification

To verify the gadget driver is working correctly:

1. **Check OnePlus logs:**
   ```bash
   ssh user@oneplus "tail -f ~/gud.log"
   ```

2. **Look for:**
   - `Framebuffer flushed` - buffer received and displayed
   - Large `compressed_length` (500KB+) - actual desktop content
   - No `Suspend` immediately after `Enable`

3. **On host, check DRM device:**
   ```bash
   ls /sys/class/drm/card0-USB-1
   cat /sys/class/drm/card0-USB-1/status  # should show "connected"
   ```

## Related Files

- `IOMMU_INVESTIGATION.md` - DMA-BUF/IOMMU investigation for OnePlus 6
- `DEPLOY.md` - Deployment instructions for postmarketOS
- `COMPARISON.md` - Kernel module vs userspace implementation comparison
