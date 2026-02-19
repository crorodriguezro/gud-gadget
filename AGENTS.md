# AGENTS.md

Guidelines for coding agents working on the gud-gadget project.

## Project Overview

GUD (Generic USB Display) gadget implementation in Rust. A userspace driver that enables a Linux device to act as a USB display using FunctionFS.

**Workspace structure:**
- `gadget/` - Library crate implementing the GUD protocol
- `drm/` - Binary crate that renders to a DRM framebuffer

## Build Commands

```bash
cargo build                              # Build all crates
cargo build --release                    # Build in release mode
cross build --release --target aarch64-unknown-linux-musl  # For postmarketOS/Alpine
```

## Lint and Format Commands

```bash
cargo clippy --all-targets --all-features -- -D warnings  # Run clippy
cargo fmt                                # Format code
cargo fmt -- --check                     # Check formatting
cargo check --all-targets --all-features # Type check
```

## Test Commands

```bash
cargo test --all                         # Run all tests
cargo test -p gud-gadget                 # Run tests for gadget crate
cargo test -p gud-drm                    # Run tests for drm crate
cargo test test_name                     # Run a single test by name
cargo test -p gud-gadget test_name       # Run a single test in specific crate
cargo test --all -- --nocapture          # Run tests with output
```

Note: This project currently has no automated tests. Consider adding unit tests for new functionality.

## Running the Binary

```bash
sudo ./target/release/gud-drm /dev/dri/card0              # On device
RUST_LOG=debug sudo ./target/release/gud-drm /dev/dri/card0  # With debug logging
RUST_LOG=trace sudo ./target/release/gud-drm /dev/dri/card0  # With trace logging
```

## Code Style Guidelines

### Imports

Group imports in order: std → external crates → internal crates, separated by blank lines.

```rust
use std::fs::File;
use std::sync::Arc;

use anyhow::{bail, Context};
use tracing::{debug, trace, warn};

use gud_gadget::{DisplayMode, Event};
```

### Naming Conventions

- **Constants**: `SCREAMING_SNAKE_CASE` - `const GUD_DISPLAY_MAGIC: u32 = 0x1d50614d;`
- **Types**: `PascalCase` - `struct PixelDataEndpoint`
- **Functions/Methods**: `snake_case` - `fn send_descriptor()`
- **Variables**: `snake_case` - short-lived bindings can use single letters

### Formatting

- Use `cargo fmt` before committing
- Max line length: 100 chars (default rustfmt)
- Indent: 4 spaces (no tabs)

### Types

- Explicit type annotations for public APIs
- Type inference for local variables when obvious
- Use specific integer types based on protocol requirements (e.g., `u16` for GUD fields)

### Error Handling

- Use `anyhow::Result` for fallible functions
- Use `.context()` to add error context: `.context("serialize display descriptor")?`
- Use `.expect()` with descriptive messages for programmer errors
- Use `bail!` for early returns with errors
- Library code should return `Result`, not panic

### Logging

Use `tracing` crate with appropriate levels:
- `trace!` - Very detailed (inside loops, per-packet)
- `debug!` - Debug info (function entry/exit, state changes)
- `warn!` - Unexpected but handled conditions
- `error!` - Errors affecting operation

Include relevant values in log messages.

### Documentation

- Use `///` for doc comments on public items
- Keep doc comments concise

### Code Organization

- Protocol constants: `gadget/src/lib.rs`
- Hardware-specific code: `drm/src/main.rs`
- Keep `drm` binary minimal, delegate logic to `gadget` library

### Unsafe Code

- Document safety with `// SAFETY:` comments
- Minimize unsafe block scope
- Prefer safe abstractions

### Comments

- Explain *why*, not *what*
- Reference relevant specs/issues with URLs

### Match Expressions

- Handle all variants explicitly when possible
- Use `_` catch-all only when appropriate
- For ignored bindings, use `_name` pattern

### Serde Usage

- Use `#[derive(Serialize, Deserialize)]` for protocol structs
- Ensure struct layout matches wire format (no padding)
- Use `ssmarshal` for fixed-size serialization

## Architecture Notes

- `gadget` crate: GUD USB display protocol implementation
- `drm` crate: DRM/KMS framebuffer rendering
- Communication: USB control endpoint (ep0) + bulk endpoint
- LZ4 compression supported for buffer transfers
