# Domovoi

Domovoi runs your coding agents (Claude Code, Codex, OpenCode, pi and Google's
Antigravity) on your own machine and lets you drive them from native Mac,
iPhone, iPad and Android apps, wherever you are.

It has two parts:

- **`domovoi-server`**: an execution server written in Rust. It speaks the same
  wire protocol as [t3code](https://github.com/pingdotgg/t3code)'s server, so
  the unmodified t3code web client works against it too.
- **Native clients**: SwiftUI apps for macOS and iOS and a Kotlin app for
  Android. All of them share one Rust client core built on
  [Crux](https://github.com/redbadger/crux).

Devices reach the server over [iroh](https://iroh.computer): QUIC dialed by
key, with hole punching and an encrypted relay fallback. You don't need an
account or port forwarding.

## Features

- **Projects and threads** for each agent, with approvals, diffs, checkpoints,
  terminals, worktrees, and source control helpers for git and GitHub PRs.
- **Your existing sessions**: folders you have worked in with Claude Code
  (`~/.claude/projects`) or Codex (`~/.codex/sessions`) show up as projects.
  New terminal sessions sync in, and continuing a thread resumes the same
  session.
- **Google Antigravity**, signed in with the server machine's own `agy`
  sign-in (`agy` keeps one Google account per user, shared with the terminal
  and the Antigravity IDE; added instances use an API key or Vertex). Sign in
  and install it from Settings > Providers in the Mac, iPhone and Android
  apps (or the t3code web client): when the browser isn't on the server's
  machine, paste Google's redirect URL back into the field. The
  account's models and thinking levels show in every client.
- **Remote access with pairing**: pair a phone once with a link or QR code. Its
  sign-in is bound to that device's own key.
- **Push notifications** on iPhone for "Needs your approval", "Finished" and
  "Failed" (bring your own APNs key).
- **A status bar icon** with remote access on/off, pairing, device list and
  launch at login.
- **The server runs on macOS and Linux.** On Linux it can run headless as a
  systemd user service. It also cross-compiles for Windows.

## Repository layout

| Path | What it is |
| --- | --- |
| `apps/server` | `domovoi-server`, the Rust execution server |
| `apps/domovoi-core` | Shared Crux client core (connection, Effect RPC, projects, threads, approvals) |
| `apps/domovoi-apple` | macOS and iOS apps, share extension, widgets and Live Activities |
| `apps/domovoi-android` | Android app (Kotlin) |
| `apps/ws-tap` | Recording reverse proxy that captures HTTP and WebSocket RPC traffic as NDJSON fixtures |
| `crates/effect-rpc` | Server side of Effect's RPC wire protocol |
| `crates/domovoi-contracts` | Wire types for t3code's server API, validated against the real Effect schemas |
| `crates/domovoi-model` | Typed records and the thread read model, shared by the server and the client core |
| `crates/domovoi-remote` | HTTP/1.1 over iroh streams, plus a loopback bridge for clients |
| `conformance/` | Schema checks and RPC test cases run against t3code's own client code |
| `fixtures/golden/` | Recorded sessions used for replay tests and benchmarks |
| `docs/` | [Remote access and deployment](docs/remote-access.md), [verification](docs/verification.md) |

## Getting started

### Requirements

- Rust (2024 edition) and [`just`](https://github.com/casey/just)
- [Git LFS](https://git-lfs.com) for the images and icons (see
  [Git LFS](#git-lfs) below).
- At least one agent CLI installed and signed in: `claude`, `codex`,
  `opencode` (2.0.18+) or `pi`. Antigravity's `agy` (1.5+) can be on the PATH
  or installed by the server.
- To build the apps: `boltffi` (`cargo install boltffi_cli`), which packages
  the Rust core for Swift and Kotlin, plus:
  - **Mac**: Xcode 26 and [XcodeGen](https://github.com/yonaskolb/XcodeGen).
    The apps need macOS 15 or later; the iPhone and iPad app needs iOS 26.
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
managing devices, launch at login, push notifications, and running on a
Linux server.

### Build the apps

```sh
just mac-run          # build and launch the Mac app (debug)
just ios-run          # build and run the iPhone app in the simulator
just ios-pair         # pair the simulator with a running server
just android-run      # build and run the Android app in the emulator
just android-pair     # pair the emulator with a running server
```

Run `just --list` to see every recipe, including packaging (`just package`,
`just server-package-linux`) and device builds. `just mac-package` requires a
Developer ID Application signing identity for `DEVELOPMENT_TEAM` (default
`LALG9U2L97`). Use the same signing team for successive releases so macOS
keeps the client's Keychain approvals. To sign for a physical iPhone with
your own team, set `DEVELOPMENT_TEAM=<team id>`.

## Testing

```sh
just test         # all Rust tests; the schema checks also run when Node 26+ is on PATH
just check        # Git LFS files present, then build everything and fail on any warning or lint finding
just rpc-groups   # every RPC method against every answer it can give (6 to 10 minutes)
```

The tests replay recorded t3code sessions against the real server binary. They
also check every request and answer against t3code's own Effect schemas.
[docs/verification.md](docs/verification.md) describes the full acceptance
setup, which runs the unmodified t3code web client against `domovoi-server`.

## License

[MIT](LICENSE). The default settings, keybindings and capabilities in
`crates/domovoi-contracts/defaults` come from t3code (MIT License, Copyright (c)
2026 T3 Tools Inc.). See its [NOTICE](crates/domovoi-contracts/defaults/NOTICE.md).
