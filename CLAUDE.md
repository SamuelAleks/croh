# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```bash
# Build all crates (debug)
cargo build

# Build release binaries
cargo build --release

# Run GUI
cargo run -p croc-gui

# Run daemon
cargo run -p croc-daemon -- <subcommand>

# Run with debug logging
RUST_LOG=debug cargo run -p croc-gui
```

## Architecture

This is a Rust workspace wrapping the external [croc](https://github.com/schollz/croc) CLI tool with a native desktop GUI and headless daemon.

### Crate Structure

- **`crates/core`** (`croc-gui-core`): Shared library used by both GUI and daemon
  - `croc/` - Subprocess wrapper: spawns croc CLI, parses stdout/stderr for events (codes, progress, completion)
  - `config.rs` - JSON config management (platform-specific paths via `dirs` crate)
  - `transfer.rs` - Transfer state machine and `TransferManager` for tracking active transfers

- **`crates/gui`** (`croc-gui`): Slint-based desktop app
  - `main.rs` - Window setup, Slint module inclusion via `slint::include_modules!()`
  - `app.rs` - All application state and UI callbacks; spawns threads with tokio runtimes for async operations
  - `ui/main.slint` - UI definition

- **`crates/daemon`** (`croc-daemon`): Headless CLI service using clap
  - Subcommands: `run`, `receive`, `status`, `peers`, `config`

### Key Patterns

**Croc Integration**: The `CrocProcess` struct (`core/src/croc/process.rs`) spawns croc as a child process, monitors stdout/stderr via tokio tasks, and emits `CrocEvent` variants through mpsc channels. The `output.rs` module contains regex parsers for croc's output format.

**GUI Threading**: Slint requires UI updates on the main thread. The app spawns `std::thread` with embedded tokio runtimes for async work, then uses `Weak<MainWindow>` to post updates back to UI.

**Config Locations**:
- Linux: `~/.config/croc-gui/config.json`
- Windows: `%APPDATA%\croc-gui\config.json`

## Reference Files

The `/misc` directory (gitignored) contains reference files that may be useful for development context.

**`misc/croc-gui-v2-plan.md`** - Master planning document for the entire project. Contains:
- Migration plan (Part One): Phases 0.1-0.10 covering Python→Rust migration
- Feature implementation plan (Part Two): Phases 1-8 for Iroh integration, trust, file push/pull
- Protocol specifications, data models, security model, UI/UX designs
- Current progress: approximately Phase 0.10 level (testing & verification)
