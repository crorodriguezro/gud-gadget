# Laptop AArch64 Build

Build the Pi `gud-drm` binary on the Fedora AArch64 laptop with the native
Fedora cross compiler. Do not use the obsolete `aarch64-linux-gnu-gcc` name or
the removed `/usr/aarch64-redhat-linux/sys-root/fc43` sysroot path.

## Prerequisites

```bash
sudo dnf install gcc-aarch64-linux-gnu
rustup toolchain install stable-aarch64-unknown-linux-gnu
rustup default stable-aarch64-unknown-linux-gnu
```

Verify the compiler is available:

```bash
aarch64-redhat-linux-gcc --version
```

## Build And Test

The checked-in `.cargo/config.toml` selects the verified compiler and prevents
the host GCC runtime from being required on the Pi. From the repository root:

```bash
cargo fmt --all -- --check
cargo test -p gud-gadget -p gud-drm -- --test-threads=1
cargo build --release -p gud-drm
sha256sum target/release/gud-drm
file target/release/gud-drm
```

The artifact must report `ELF 64-bit ... ARM aarch64` and use the
`/lib/ld-linux-aarch64.so.1` interpreter before deployment.

## Verified 2026-07-31

The lifecycle-instrumented build was produced with `rustc 1.93.1` and
`aarch64-redhat-linux-gcc 16.1.1`:

```text
cargo test -p gud-gadget -p gud-drm -- --test-threads=1: 116 passed
target/release/gud-drm SHA-256:
7b7f74a44ce12a4f2bfbc202e6ccbb3548dd39c73996fd1e8b2b7106caa00409
```

This hash identifies only that local build. Recalculate and record the hash
after every rebuild and after deployment to the Pi.
