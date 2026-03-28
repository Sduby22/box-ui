# Box UI

A lightweight, cross-platform GUI wrapper for [sing-box](https://github.com/SagerNet/sing-box), built with Rust and egui.

## Project Goals

Box UI aims to provide a minimal yet functional graphical interface for managing sing-box, replacing the need for command-line operation. The focus is on simplicity and low resource usage.

## UI Layout

Left sidebar navigation + content area. Sidebar bottom always shows core status and real-time speed.

```
┌──────────┬─────────────────────────────┐
│          │                             │
│ Dashboard│   [Active tab content]      │
│ Outbounds│                             │
│ Connections                            │
│ Logs     │                             │
│ Settings │                             │
│          │                             │
│──────────│                             │
│ ● Running│                             │
│ ↑1.2 ↓3.4│                             │
└──────────┴─────────────────────────────┘
```

### Tabs

| Tab | Content |
|---|---|
| **Dashboard** | Traffic speed line chart (egui_plot) + Core management (version, start/stop/restart, download kernel) + Config management (import local, add remote subscription, switch/edit/delete configs) |
| **Outbounds** | List proxy groups and available nodes via Clash API; switch active node per group |
| **Connections** | Table of active connections with sorting: process, host, chain, rule, upload/download totals and speeds |
| **Logs** | Streaming log output from core via WebSocket, with level filtering (debug / info / warn / error) and ANSI color support |
| **Settings** | Autostart toggle, launch core on start toggle, privileged helper daemon management, about info |

## Core Features

### 1. Configuration Management
- Import configuration files from local filesystem
- Add remote subscription URLs and pull configs
- Edit/delete/switch between multiple configurations
- Configs tracked by UUID with auto-migration

### 2. Core (Kernel) Management
- Download specific versions from GitHub Releases (https://github.com/SagerNet/sing-box/releases)
- Start / stop / restart the sing-box process
- Switch between installed versions
- Display current core version and running status
- **Working directory**: All kernel launch modes use a dedicated `pwd/` directory inside the app's data folder
- **Run as Admin**: Optional elevated execution with three backends:
  - Direct elevated: platform-specific password prompts (sudo/pkexec/UAC)
  - Helper daemon: one-time install, no password on subsequent starts
- **Privileged Helper Daemon**: One-time install of a root-level helper service
  - macOS: launchd LaunchDaemon + Unix socket (implemented)
  - Linux: systemd service + Unix socket (planned)
  - Windows: Windows Service + Named pipe (planned)
  - Auto-update: GUI updates helper binary when versions mismatch
  - PID binding: helper auto-exits when GUI process dies

### 3. Real-time Monitoring (via sing-box Clash API over WebSocket)
- Traffic speed line chart (`/traffic`)
- Outbound node selection (`/proxies`, `/proxies/:group`)
- Active connection list with per-connection speed calculation (`/connections`)
- Log streaming with level filtering (`/logs`)

### 4. Autostart Management
- Register/unregister the GUI app as a login startup item
- **Launch core on start**: optionally auto-start the sing-box core when the app launches
- Platform-specific implementations:
  - **Linux**: XDG autostart (~/.config/autostart/*.desktop)
  - **macOS**: LaunchAgent plist
  - **Windows**: Startup folder .bat script

### 5. System Tray
- Tray icon with context menu (Show / Quit)
- Minimize to tray on window close
- Quit handler: stops kernel and shuts down helper before exit

### 6. Cross-Platform Support
- Linux (X11 / Wayland)
- macOS
- Windows

## Tech Stack

- **Language**: Rust (latest stable)
- **UI Framework**: [egui](https://github.com/emilk/egui) via [eframe](https://github.com/emilk/egui/tree/master/crates/eframe)
- **System Tray**: [tray-icon](https://github.com/tauri-apps/tray-icon)
- **Charts**: [egui_plot](https://github.com/emilk/egui_plot) (traffic speed line chart)
- **Tables**: [egui_extras](https://github.com/emilk/egui/tree/master/crates/egui_extras) (connections table with sortable columns)
- **HTTP Client**: reqwest (for remote config fetching, GitHub API & sing-box Clash API)
- **WebSocket**: tokio-tungstenite (traffic, connections, logs streaming)
- **Async Runtime**: tokio
- **Serialization**: serde + serde_json (sing-box configs are JSON)
- **Process Management**: std::process::Command / tokio::process
- **IPC**: Unix domain sockets (macOS/Linux) / Named pipes (Windows) with 4-byte length-prefixed JSON (box-ui-ipc crate)
- **Archive Extraction**: flate2 + tar (tar.gz), zip (zip archives) for kernel downloads
- **File Dialogs**: rfd (native file picker)
- **Autostart**: Platform-specific implementations (XDG autostart / LaunchAgent / shell:startup)

## Project Structure

```
box-ui/                        # Cargo workspace root
├── src/                       # GUI app
│   ├── main.rs                # Entry point
│   ├── app.rs                 # Main eframe::App, tray, toast system
│   ├── ui/                    # UI components/panels
│   │   ├── mod.rs
│   │   ├── dashboard.rs       # Dashboard: traffic chart, core mgmt, config mgmt
│   │   ├── outbounds.rs       # Outbound proxy group & node selector
│   │   ├── connections.rs     # Active connection table with sorting & speed
│   │   ├── logs.rs            # Log viewer with ANSI color parsing
│   │   └── settings.rs        # Autostart, launch-on-start, helper, about
│   └── core/                  # Business logic
│       ├── mod.rs             # Module exports + shared format_speed utility
│       ├── kernel.rs          # sing-box process management (direct + elevated + helper)
│       ├── download.rs        # GitHub release fetching, archive extraction, remote config
│       ├── autostart.rs       # Autostart registration (per-platform)
│       ├── settings.rs        # AppSettings persistence & SettingsManager
│       ├── helper_client.rs   # GUI-side IPC client for helper daemon
│       └── helper_install.rs  # Helper daemon install/uninstall/update (per-platform)
├── box-ui-helper/             # Privileged helper daemon (runs as root/SYSTEM)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs            # Entry point (dispatches to unix or windows)
│       ├── handler.rs         # Request handling, process lifecycle, GUI liveness check
│       └── unix.rs            # Unix socket listener, peer verification, chown
├── box-ui-ipc/                # Shared IPC protocol types
│   ├── Cargo.toml
│   └── src/lib.rs             # Request/Response enums, wire protocol, platform constants
├── Cargo.toml
├── CLAUDE.md
└── README.md
```

## Data Directory

App data is stored in `~/.local/share/box-ui/` (or platform equivalent via `dirs::data_dir()`):

```
box-ui/
├── settings.json    # App settings (configs, kernels, preferences)
├── configs/         # Imported/downloaded sing-box config files
├── kernels/         # Downloaded sing-box binaries
└── pwd/             # Working directory for sing-box process
```

## Build & Run

```bash
cargo run            # Debug build
cargo build --release  # Release build
```

## Development Rules

- **Adding dependencies**: Always use `cargo add <crate>` instead of manually editing `Cargo.toml`.
- **Linting**: Use `cargo clippy` for code checks and fix all warnings before committing.
- **Post-modification review**: After each code change, review the modified code and its surrounding context for engineering or performance anti-patterns and fix them. Examples include but are not limited to: unnecessary cloning, redundant allocations, blocking calls in async contexts, missing error propagation, overly broad trait bounds, inefficient data structures, and dead code. Run `cargo clippy` after changes to catch additional issues.
- **Keep CLAUDE.md in sync**: When a modification changes the project structure, tech stack, features, or development conventions described in this file, update CLAUDE.md accordingly to keep it accurate.

## Differences from GUI for SingBox

[GUI for SingBox](https://github.com/nicehash/GUI.for.SingBox) takes a more interventionist approach — it distinguishes between rule subscriptions and node subscriptions, and the configuration file actually fed to the sing-box core is assembled and generated by the GUI itself (splicing rules, nodes, and overrides together).

Box UI takes a fundamentally different philosophy: **the GUI never modifies or overrides the user's configuration**. Specifically:

- **No config manipulation**: Box UI will not toggle Tun mode, inject DNS settings, merge rule sets, or alter any field in the config. What the user imports or pulls from a remote URL is exactly what gets passed to sing-box.
- **No rule/node separation**: There is no concept of "rule subscriptions" vs "node subscriptions". A configuration is a single, complete sing-box config file — the user is fully responsible for its content.
- **Pure passthrough**: Box UI is a transparent wrapper. It stores, switches, and delivers configs to the core, but never generates or transforms them.

This makes Box UI simpler and more predictable — the user always knows exactly what config the core is running, with no hidden overrides or magic merging.

## Design Principles

- **Lightweight**: Minimal resource footprint; egui renders natively, no embedded browser
- **Simple**: Provide only essential controls, avoid feature creep
- **Portable**: Single binary with embedded assets where possible
- **Safe**: No sudo/admin required for GUI operations; elevate only when necessary for core operations
