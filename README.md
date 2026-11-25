# Domovoi

Domovoi runs your coding agents (Claude Code, Codex, OpenCode, pi, Google's
Antigravity and any [ACP](https://agentclientprotocol.com) agent) on your own
machine and lets you drive them from native Mac, iPhone, iPad, Android, Linux
and Windows apps, wherever you are.

It has two parts:

- **`domovoi-server`**: an execution server written in Rust. It speaks the same
  wire protocol as [t3code](https://github.com/pingdotgg/t3code)'s server, so
  the unmodified t3code web client works against it too.
- **Native clients**: SwiftUI apps for macOS, iOS and iPadOS, a Kotlin app for
  Android, and a Rust desktop app built with [Iced](https://iced.rs) for Linux,
  Windows and macOS. All of them share one Rust client core built on
  [Crux](https://github.com/redbadger/crux).

Devices reach the server over [iroh](https://iroh.computer): QUIC dialed by
key, with hole punching and an encrypted relay fallback. You don't need an
account or port forwarding.

## Features

- **Projects and threads** for each agent, with approvals, diffs, checkpoints,
  terminals, worktrees, custom project icons, and source control helpers for
  git, GitHub pull requests (`gh`) and GitLab merge requests (`glab`).
- **Issues** from GitHub, GitLab and Linear beside each project, linked to the
  threads that work on them.
- **Your existing sessions**: folders you have worked in with Claude Code
  (`~/.claude/projects`) or Codex (`~/.codex/sessions`) show up as projects.
  New terminal sessions sync in, and continuing a thread resumes the same
  session.
- **Google Antigravity**, signed in with the server machine's own `agy`
  sign-in (`agy` keeps one Google account per user, shared with the terminal
  and the Antigravity IDE; added instances use an API key or Vertex). Sign in
  and install it from Settings > Providers in any client (or the t3code web
  client): when the browser isn't on the server's machine, paste Google's
  redirect URL back into the field. The account's models and thinking levels
  show in every client.
- **ACP agents and registry**: add any Agent Client Protocol agent by command
  or install one from the ACP registry (platform binary, npm or `uvx`), plus
  t3code's Grok, Kiro and fx drivers.
- **Scheduled tasks and webhooks**: run prompts on an interval, at fixed times
  of day and weekdays, once at a set time, or when a webhook URL
  (`/api/hooks/…`) receives a request (with HMAC-SHA256 signature verification,
  filters, deduplication and coalescing).
- **Built-in MCP server for agents**: running agents reach `/mcp` (or a stdio
  bridge for ACP agents) to read and message sibling threads, delegate tasks to
  child agents across providers or worktrees, hand off to worktrees, schedule
  tasks, publish artifacts, link issues and pull requests, ask the user for a
  secret (never stored in the conversation) and drive a headless browser.
- **Preview**: a server-side headless browser (a local Chrome, Chromium or
  Edge) streamed to clients, and a tunnel that opens the server machine's
  local dev servers on other devices.
- **Artifacts, diagrams and images**: versioned interactive artifacts (HTML and
  multi-file workspace bundles, React/JSX previews, SVG, Mermaid, Markdown and
  code), server-rendered Mermaid diagrams, and inline workspace and tool image
  previews including Git LFS objects.
- **Usage limits and recovery**: tracks rate-limit windows for Claude Code,
  Codex and Antigravity across clients, and can snooze and resume threads once
  a limit resets.
- **Remote access with pairing**: pair a device once with a link or QR code.
  Its sign-in is bound to that device's own key.
- **Push notifications** for "Needs your approval", "Finished" and "Failed": on
  iPhone through APNs, plus Live Activities following the current turn (bring
  your own APNs key); on Android through any UnifiedPush distributor (no Google
  services).
- **Linked servers**: link another Domovoi server over iroh, read-only or with
  full access, and list its threads from here.
- **Managed policy**: an administrator's policy file locks settings and turns
  off remote access, webhook tasks, providers or the preview tunnel.
- **A status bar icon** with remote access on/off, pairing, device list and
  launch at login.
- **The server runs on macOS, Linux and Windows.** It can run headless as a
  systemd user service on Linux, or as a scheduled task on Windows
  ([docs/windows.md](docs/windows.md)).
  `domovoi-server doctor` checks an install, and `/health` and `/ready` serve
  monitoring.

## Repository layout

| Path | What it is |
| --- | --- |
| `apps/server` | `domovoi-server`, the Rust execution server |
| `apps/domovoi-core` | Shared Crux client core (connection, Effect RPC, projects, threads, approvals) |
| `apps/domovoi-apple` | macOS and iOS apps, share extension, widgets and Live Activities |
| `apps/domovoi-android` | Android app (Kotlin) |
| `apps/domovoi-iced` | `domovoi-desktop`, the cross-platform desktop app for Linux, Windows and macOS (Iced) |
| `apps/domovoi-windows` | `Domovoi.exe`, the native Windows app (WinUI 3 on `windows-reactor`, in progress) |
| `apps/server-launcher` | `domovoi-server-launcher.exe`: starts the Windows server with no console window, and restarts it after a failure |
| `packaging/windows` | The Windows installers (WiX MSIs) |
| `apps/ws-tap` | Recording reverse proxy that captures HTTP and WebSocket RPC traffic as NDJSON fixtures |
| `crates/effect-rpc` | Server side of Effect's RPC wire protocol |
| `crates/domovoi-contracts` | Wire types for t3code's server API, validated against the real Effect schemas |
| `crates/domovoi-model` | Typed records and the thread read model, shared by the server and the client core |
| `crates/domovoi-remote` | HTTP/1.1 over iroh streams, plus a loopback bridge for clients |
| `crates/domovoi-desktop-host` | Runs the client core's effects on Tokio, with secure storage and `domovoi://` link hand-off, for the Rust desktop clients |
| `conformance/` | Schema checks and RPC test cases run against t3code's own client code |
| `fixtures/golden/` | Recorded sessions used for replay tests and benchmarks |
| `tools/` | Icon generator, binary size report, cross-build and Linux deployment scripts |
| `docs/` | [Remote access and deployment](docs/remote-access.md), [verification](docs/verification.md) |

## Getting started

### Requirements

- Rust (2024 edition) and [`just`](https://github.com/casey/just)
- [Git LFS](https://git-lfs.com) for the images and icons (see
  [Git LFS](#git-lfs) below).
- At least one agent CLI installed and signed in: `claude`, `codex`,
  `opencode` (2.0.18+) or `pi`. Antigravity's `agy` (1.5+) can be on PATH or
  installed by the server, and ACP agents can be installed from the registry.
- On Linux, building `domovoi-server` with its default status bar icon (`tray`
  feature) needs GTK 3, `libxdo` and an appindicator library; build headless
  with `--no-default-features` to leave them out.
- **Desktop app (`domovoi-iced`, Linux / Windows / macOS)**: Rust only.
- **Windows app and installers**: MSVC, the Windows SDK, PowerShell 7 and the
  .NET SDK. [docs/windows.md](docs/windows.md) lists them; `just windows-doctor`
  checks them.
- **Apple and Android apps**: `boltffi` (`cargo install boltffi_cli`), which
  packages the Rust core for Swift and Kotlin, plus:
  - **Mac / iOS**: Xcode 26 and [XcodeGen](https://github.com/yonaskolb/XcodeGen).
    The Mac app needs macOS 15 or later; the iPhone and iPad app needs iOS 26.
  - **Android**: the Android SDK and NDK. The app needs Android 10 (API 29)
    or later.

### Git LFS

Raster images and icon files (`.png`, `.jpg`, `.ico`, `.icns` and the rest
listed in `.gitattributes`) are stored in Git LFS: the app icons in
`apps/domovoi-iced/assets` and the server's Windows icon in `apps/server/icon`.
SVGs (the Lucide icons and the `.icon` layers) and fonts are plain Git files,
so they diff and a checkout without LFS still draws its icons and text.

```sh
git lfs install   # once per machine, before cloning
git lfs pull      # in a clone made without it
just lfs-check    # every LFS file is present, not a pointer file
```

A checkout that only has pointer files still builds, without its window, Dock
and Windows icons (the build prints a warning). `just test` fails and says to
run `git lfs pull`. `just check` and the packaging recipes run `just lfs-check`
first, so a release never ships pointer text.

`tools/icons.py` draws the app icons. `just icons` writes the `.icon`
documents and the Android drawables; `just icons export` also renders the PNGs
and `.ico` files kept in LFS (needs Xcode 26 and Pillow).

### Run the server

```sh
cargo run -p domovoi-server            # listens on 127.0.0.1:13773, state in ~/.domovoi
cargo run -p domovoi-server -- --tray  # with the status bar icon
```

The server prints a pairing URL at start. To reach it from your other devices,
turn remote access on (you only need to do this once) and pair:

```sh
domovoi-server --remote
domovoi-server pair --remote           # prints a domovoi:// link and a QR code
```

For development, `just server`, `just pair` and `just tray` do the same with a
throwaway home in `.reference/domovoi-home`.

[docs/remote-access.md](docs/remote-access.md) covers using your own relay,
managing devices, launch at login, push notifications, webhook tasks, and
running on a Linux server.

### Build the apps

```sh
just desktop          # build and launch the Iced desktop app (Linux, Windows, macOS; release)
just desktop-dev      # same, debug build
just mac-run          # build and launch the native Mac app (debug)
just ios-run          # build and run the iPhone app in the simulator
just ios-pair         # pair the simulator with a running server
just android-run      # build and run the Android app in the emulator
just android-pair     # pair the emulator with a running server
```

Run `just --list` to see every recipe, including packaging (`just package`,
`just desktop-package`, `just server-package-linux`) and device builds.
`just mac-package` requires a Developer ID Application signing identity for
`DEVELOPMENT_TEAM` (default `LALG9U2L97`). Use the same signing team for
successive releases so macOS keeps the client's Keychain approvals. To sign for
a physical iPhone with your own team, set `DEVELOPMENT_TEAM=<team id>`.
`just testflight` uploads the iPhone app to TestFlight; see
[docs/testflight.md](docs/testflight.md).

## Testing

```sh
just test          # all Rust tests; the schema checks also run when Node 26+ is on PATH
just check         # Git LFS files present, then build everything and fail on any warning or lint finding
just desktop-test  # Iced desktop unit tests and headless UI snapshot flows
just android-test  # Kotlin unit tests for the Android shell
just rpc-groups    # every RPC method against every answer it can give (6 to 10 minutes)
```

The tests replay recorded t3code sessions against the real server binary. They
also check every request and answer against t3code's own Effect schemas.
[docs/verification.md](docs/verification.md) describes the full acceptance
setup, which runs the unmodified t3code web client against `domovoi-server`.

## License

[MIT](LICENSE). The default settings, keybindings and capabilities in
`crates/domovoi-contracts/defaults` come from t3code (MIT License, Copyright (c)
2026 T3 Tools Inc.). See its [NOTICE](crates/domovoi-contracts/defaults/NOTICE.md).
