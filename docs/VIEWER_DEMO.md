# Viewer Demo

The `qt` branch adds a windowed phone demo so the GUD gadget can run without stopping the phone desktop environment.

## Components

- `viewer-ipc`
  - shared JSON control messages and default runtime paths
- `gud-viewerd`
  - privileged daemon
  - owns the USB gadget and GUD protocol
  - keeps one RGB565 shared framebuffer in `/run/user/<uid>/gud-viewer/framebuffer.bin`
  - publishes session/frame notifications on `/run/user/<uid>/gud-viewer/control.sock`
- `gud-viewer-gtk`
  - GTK4/libadwaita windowed viewer
  - maps the shared framebuffer and renders it inside the normal phone session

## Build

Default workspace build:

```bash
cargo build
cargo test -p viewer-ipc -p gud-viewerd -p gud-gadget -p gud-drm -p gud-viewer-gtk
```

GTK viewer binary:

```bash
cargo build -p gud-viewer-gtk --features gtk-runtime
```

On Fedora, the GTK build needs the development packages that provide:

- `gtk4.pc`
- `libadwaita-1.pc`
- `glib-2.0.pc`
- `gio-2.0.pc`
- `gobject-2.0.pc`
- `pango.pc`
- `cairo.pc`

The local workstation used during implementation did not have those `pkg-config` files installed, so the daemon path was build-tested but the GTK binary could not be linked locally yet.

## Runtime

Cross-build and deploy the daemon for the phone:

```bash
cross build --release --target aarch64-unknown-linux-musl -p gud-viewerd
scp target/aarch64-unknown-linux-musl/release/gud-viewerd cristian@192.168.1.106:/home/cristian/gud-viewerd
```

Launch the daemon on the phone while leaving the desktop environment running:

```bash
./scripts/run-oneplus-viewer-demo.sh
```

The helper script:

- stops and masks `usb-moded`
- starts `gud-viewerd` with `Restart=on-failure`
- leaves `greetd`, `phosh`, and the compositor alone

Then launch the viewer app in the phone desktop session:

```bash
/home/cristian/gud-viewer-gtk
```

## Status

Implemented and build-tested:

- `viewer-ipc`
- `gud-viewerd`
- launch helper script
- `gud-viewer-gtk` source tree with opt-in build gating

Validated locally:

- `cargo build`
- `cargo test -p viewer-ipc -p gud-viewerd -p gud-gadget -p gud-drm -p gud-viewer-gtk`
- `cross build --release --target aarch64-unknown-linux-musl -p gud-viewerd`

Not yet fully validated end to end:

- linking `gud-viewer-gtk` on the current workstation
- running the viewer app binary on the phone session
