# Development

Architecture, repository layout, build prerequisites, packaging, testing, and
platform implementation notes for Domovoi contributors. For everyday setup and
usage, see [README.md](../README.md).

## Architecture

Domovoi has two halves:

- **`domovoi-server`** (`apps/server`): an execution server written in Rust. It
  manages projects, threads, SQLite state, PTY terminals, git/PR/issue helpers,
  agent adapters, scheduled tasks/webhooks, the headless preview browser, and
  monitoring endpoints (`/health` and `/ready`).
- **Native clients** sharing one Rust client core (`apps/domovoi-core`) built on
  [Crux](https://github.com/redbadger/crux):
  - **Apple** (`apps/domovoi-apple`): SwiftUI apps for macOS, iOS, and iPadOS,
    plus widgets, Share extension, and Live Activities (core packaged via
    `boltffi`).
  - **Android** (`apps/domovoi-android`): Kotlin app (core packaged via
    `boltffi`).
  - **Cross-platform desktop** (`apps/domovoi-iced`): `domovoi-desktop`, built
    with [Iced](https://iced.rs) for Linux, Windows, and macOS.
  - **Windows native** (`apps/domovoi-windows`): `Domovoi.exe`, a pure-Rust
    WinUI 3 app on `windows-reactor`.

Networking and integration highlights:

- **Remote transport** (`crates/domovoi-remote`): devices reach the server over
  [iroh](https://iroh.computer) — QUIC dialed by key, with hole punching and an
  encrypted relay fallback — carrying HTTP/1.1 over iroh streams plus a loopback
  bridge for clients.
- **Same-machine auth** (`crates/domovoi-local`): the server writes a descriptor
  and a local desktop client proves it runs as the same user by writing a
  one-time request file.
- **Built-in MCP server for agents**: running agents connect to `/mcp`
  (Streamable HTTP) or a stdio bridge (`domovoi-server acp-mcp-bridge`) for ACP
  agents.

## Repository layout

| Path | What it is |
| --- | --- |
| `apps/server` | `domovoi-server`, the Rust execution server |
| `apps/domovoi-core` | Shared Crux client core (connection, RPC, projects, threads, approvals) |
| `apps/domovoi-apple` | macOS and iOS apps, share extension, widgets and Live Activities |
| `apps/domovoi-android` | Android app (Kotlin) |
| `apps/domovoi-iced` | `domovoi-desktop`, the cross-platform desktop app for Linux, Windows and macOS (Iced) |
| `apps/domovoi-windows` | `Domovoi.exe`, the native Windows app (WinUI 3 on `windows-reactor`) |
| `apps/server-launcher` | `domovoi-server-launcher.exe`: starts the Windows server with no console window, and restarts it after a failure |
| `packaging/windows` | The Windows installers (WiX MSIs) |
| `crates/domovoi-rpc` | The RPC socket between the server and its clients: messages, the connection loop, packed frames |
| `crates/domovoi-api` | Every call the server answers, defined once: its name, kind, permission and who calls it |
| `crates/domovoi-model` | Typed records and the thread read model, shared by the server and the client core |
| `crates/domovoi-remote` | HTTP/1.1 over iroh streams, plus a loopback bridge for clients |
| `crates/domovoi-local` | How a client finds and signs in to the server on the same machine: the descriptor the server writes and the request files a client proves itself with |
| `crates/domovoi-desktop-host` | Runs the client core's effects on Tokio, with secure storage and `domovoi://` link hand-off, for the Rust desktop clients |
| `apps/server/tests/cases/` | The RPC test cases: every method called for the answers and failures it can give |
| `fixtures/` | A recorded session of the server (`native/`) and of provider CLIs (`provider/`), for tests and benchmarks |
| `tools/` | Icon generator, binary size report, performance comparison (`perf.py`), parity checker (`parity.py`), cross-build and Linux deployment scripts |
| `docs/` | [Remote access](remote-access.md), [Windows guide](windows.md), [upgrading from T3 Code's protocol](upgrading.md), [verification](verification.md), [performance](performance.md), [TestFlight](testflight.md), [Windows hardening](windows-hardening.md) |

## Requirements

- Rust (2024 edition) and [`just`](https://github.com/casey/just).
- [Git LFS](https://git-lfs.com) for raster images and icons (see
  [Git LFS and icons](#git-lfs-and-icons)).
- At least one agent CLI installed and signed in: `claude`, `codex`, `opencode`
  (2.0.18+), or `pi`. Antigravity's `agy` (1.5+) can be on `PATH` or installed
  by the server, and ACP agents can be installed from the registry.
- **Linux server status bar icon**: `domovoi-server` enables the `tray` Cargo
  feature by default, which needs GTK 3, `libxdo`, and an appindicator library
  to build (`libgtk-3-dev libxdo-dev libayatana-appindicator3-dev` on Debian).
  Build headless with `cargo build --no-default-features` to leave them out.
- **Desktop app (`domovoi-iced`, Linux / Windows / macOS)**: Rust only.
- **Apple and Android apps**: `boltffi` (`cargo install boltffi_cli`), which
  packages the Rust core for Swift and Kotlin, plus:
  - **macOS / iOS**: Xcode 26 and [XcodeGen](https://github.com/yonaskolb/XcodeGen).
    The Mac app needs macOS 15 or later; the iPhone and iPad app needs iOS 26.
  - **Android**: the Android SDK and NDK. The app needs Android 10 (API 29) or
    later.
- **Windows app and installers**: see [Windows prerequisites](#windows-prerequisites)
  below; `just windows-doctor` checks them.

### Windows prerequisites

| Tool | Why | Get it |
| --- | --- | --- |
| Rust (MSVC toolchain, `x86_64-pc-windows-msvc`) | everything | rustup |
| Visual Studio Build Tools + Windows SDK | the linker, `rc.exe` for the icons | Visual Studio Installer, "Desktop development with C++" |
| Git for Windows (with `usr/bin` on PATH) | the justfile's shell, `cygpath` | winget `Git.Git` |
| Git LFS | the icons | `git lfs install && git lfs pull` |
| Python 3 | `just parity`, the server tests' fake CLIs | winget `Python.Python.3.13` |
| PowerShell 7 | the packaging script | winget `Microsoft.PowerShell` |
| .NET SDK 8+ | WiX, pinned in `.config/dotnet-tools.json` | winget `Microsoft.DotNet.SDK.9`, then `dotnet tool restore` |
| `just` | the recipes | winget `Casey.Just` |

The first build of the WinUI app downloads the Windows App SDK runtime
(`windows-reactor-setup`, about 60 MB) and stages it beside `Domovoi.exe`.
Nothing is installed system-wide.

## Git LFS and icons

Raster images and icon files (`.png`, `.jpg`, `.ico`, `.icns` and the rest
listed in `.gitattributes`) are stored in Git LFS: the app icons in
`apps/domovoi-iced/assets` and the server's Windows icon in `apps/server/icon`.
SVGs (the Lucide icons and the `.icon` layers) and fonts are plain Git files, so
they diff and a checkout without LFS still draws its icons and text.

```sh
git lfs install   # once per machine, before cloning
git lfs pull      # in a clone made without it
just lfs-check    # every LFS file is present, not a pointer file
```

A checkout that only has pointer files still builds, without its window, Dock,
and Windows icons (the build prints a warning). `just test` fails and says to
run `git lfs pull`. `just check` and the packaging recipes run `just lfs-check`
first, so a release never ships pointer text.

`tools/icons.py` draws the app icons. `just icons` writes the `.icon` documents
and the Android drawables; `just icons export` also renders the PNGs and `.ico`
files kept in LFS (macOS only: needs Xcode 26's `ictool` and Pillow; on Windows
the `.ico` files come from Git LFS).

## Running from source

### Server

```sh
cargo run -p domovoi-server            # listens on 127.0.0.1:13773, state in ~/.domovoi
cargo run -p domovoi-server -- --tray  # with the status bar icon
```

For development, `just server`, `just pair`, `just tray`, and `just remote …`
run against a throwaway home in `.reference/domovoi-home` on port `13774`.

### Clients

```sh
just desktop          # build and launch the Iced desktop app (Linux, Windows, macOS; release)
just desktop-dev      # same, debug build (points at `just server`'s throwaway home)
just mac-run          # build and launch the native Mac app (debug)
just ios-run          # build and run the iPhone app in the simulator
just ios-pair         # pair the simulator with a running server
just android-run      # build and run the Android app in the emulator
just android-pair     # pair the emulator with a running server
just windows-run      # build and launch the native WinUI 3 app (Windows, debug)
```

Run `just --list` to see every recipe.

### The iPhone app on a physical device

The Xcode project is generated (`just mac-generate`) and signs the iPhone app
with automatic signing for the team in the justfile (`DEVELOPMENT_TEAM=<team>`
picks another one; the bundle id is then `com.crayen.domovoi` on that team):

- `just ios-device` builds a debug app for a physical iPhone.
- `just ios-device-run <device>` also installs and launches it
  (`xcrun devicectl list devices` names the phone).
- `just ios-archive` makes a release archive in `dist/`.
- `just ios-open` opens the project in Xcode.

## Packaging, signing and cross-building

### macOS

- `just tray-app` builds `dist/Domovoi Server.app` (no Dock icon) to move to
  `/Applications`. Building the Mac app builds it too: `just mac-build` puts a
  debug copy beside `Domovoi.app` in
  `apps/domovoi-apple/build/Build/Products/Debug`, and `just mac-package` ships
  an Apple silicon one in `dist/` (its own `.zip`, and in the `.dmg` next to
  `Domovoi.app`). Its executable is `domovoi-server` itself, which starts with
  the icon and without a pairing code when launched as an app's executable. A
  launcher script that `exec`s the server would not do: macOS 26 then hides the
  icon.
- `just mac-package` requires a Developer ID Application signing identity for
  `DEVELOPMENT_TEAM` (default `LALG9U2L97`). Use the same signing team for
  successive releases so macOS keeps the client's Keychain approvals.
- `just package` (on macOS) runs `mac-package` and `server-package` and writes
  `dist/SHA256SUMS`.
- `just testflight` uploads the iPhone app to TestFlight; see
  [docs/testflight.md](testflight.md).

### Cross-platform desktop (Iced)

- `just desktop-package` packages `domovoi-desktop` into `dist/` for the current
  OS: `.app`/`.zip` on macOS, `.tar.gz` with `.desktop` and `install.sh` on
  Linux, or `.exe` on Windows.

### Linux server cross-build and SSH deployment

From macOS, against `DOMOVOI_LINUX_HOST` (an SSH destination; the justfile names
a default):

```sh
just linux-deploy        # cross-build, copy to ~/.local/bin, service install, wait until it is up
just linux-pair          # a pairing link and QR code, drawn in this terminal (valid 10 minutes)
just linux remote status # any subcommand: remote devices, service status, stop…
just linux-logs          # journalctl --user -u domovoi-server -f
```

`just server-package-linux` makes a tarball in `dist/` instead. The Linux build
from macOS (`tools/cross/linux.sh`) uses zig as the C toolchain
(`cargo install cargo-zigbuild`, `rustup target add x86_64-unknown-linux-gnu`),
needs glibc 2.34 or newer on the target machine, and leaves out the status bar
icon (`--no-default-features`), so it needs no GTK.

- `just check-linux` builds and tests the server crates on Linux in Docker.
- `just check-windows` cross-builds the server for Windows with zig
  (`cargo install cargo-zigbuild`, `rustup target add x86_64-pc-windows-gnu`).

### Windows installers

Run on native Windows (`just --list` lists all `windows-*` recipes):

```powershell
just windows-installers          # both MSIs and SHA256SUMS in dist\windows
just windows-package             # only the full one
just windows-package-server      # only the server's
just windows-install             # install the built full MSI for this user (log in target\)
just windows-uninstall
just windows-installer-cycle     # install, upgrade, downgrade, repair, remove: each step checked
just windows-server-package      # the server and launcher as a zip, for no installer at all
```

`packaging/windows/package.ps1` does the work. It:

1. Builds the release programs with the C runtime linked in, so nothing else
   needs installing. They go to `target\windows-package`, a target folder of its
   own.
2. Checks no program needs `VCRUNTIME140.dll`, then stages the programs and the
   WinUI app's runtime.
3. Signs the programs and the MSIs when `DOMOVOI_SIGN_CERT_THUMBPRINT` names a
   certificate in the user's store. `DOMOVOI_SIGN_TIMESTAMP_URL` adds a
   timestamp.
4. Builds each MSI with the pinned WiX (6.0.2, with its UI and util extensions)
   and validates it.
5. Unpacks each MSI as an administrator would, starts the unpacked server
   (`--version`), and checks the unpacked WinUI app opens its window.

`DOMOVOI_RELEASE_VERSION` overrides the version, which otherwise comes from
`Cargo.toml`. The CI workflow `.github/workflows/windows-installers.yml` runs on
`v*` tags and by hand. It signs when the `WINDOWS_CERT_PFX` (base64) and
`WINDOWS_CERT_PASSWORD` secrets are set.

`just windows-installer-cycle` checks install, upgrade, downgrade, repair,
feature retention, exclusivity between the full and server-only MSIs, and
uninstall. It builds the MSIs at the current version and the next, and refuses
to run while Domovoi is installed.

> **Note when testing registry writes inside the Claude desktop app on Windows:**
> look at the registry from outside it (for example through a scheduled task).
> That app is packaged, so what its child processes write under
> `HKCU\Software\Classes` goes to a private copy that only they see.

## Testing and benchmarks

```sh
just test          # all Rust tests
just check         # Git LFS files present, parity, build everything and fail on any warning or lint finding
just parity        # verify every client uses every core event and view field (bar tools/parity-allowlist.toml)
just desktop-test  # Iced desktop unit tests and headless UI snapshot flows
just android-test  # Kotlin unit tests for the Android shell
just rpc-groups    # every RPC method against every answer it can give (6 to 10 minutes)
just perf-pairs before after   # two kept builds measured in alternation (docs/performance.md)
```

On Windows:

```powershell
just windows-smoke               # the WinUI app pairs with `just server` and must connect
just windows-check               # the workspace builds with no warning; the lint gate
just windows-test                # unit tests: WinUI app, host, launcher, Iced app, server
just windows-test-server         # the server's integration tests that pass on Windows
just windows-snapshots           # saves a picture of each WinUI screen in target/ui-snapshots/windows
```

The Windows test recipes set `core.autocrlf=false` for the git they run. Git for
Windows turns line-ending conversion on for every repository, and the tests' own
repositories expect files written byte for byte.

The server's tests run the real binary: every RPC method is called for the
answers and failures it can give, and scripted stand-ins for the provider CLIs
play whole turns. The client core is driven the way a shell drives it, from a
session recorded from the server. [docs/verification.md](verification.md)
describes the checks; [docs/performance.md](performance.md) describes how a
change is measured.

## Windows implementation notes

### The WinUI app

`apps/domovoi-windows` is a pure-Rust WinUI 3 app on the `windows-reactor` fork
that `lwa_fm` builds on (`Cargo.toml`, `[workspace.dependencies]`). The fork
pins one rev for every `windows-rs` crate.

- The client core (`domovoi_core`) is linked directly. Its effects run in the
  host it shares with the Iced app (`crates/domovoi-desktop-host`): HTTP,
  WebSocket, the iroh tunnel, the keychain store, and notifications.
- The app is unpackaged and self-contained. `apps/domovoi-windows/AGENTS.md`
  maps the modules and gives the reactor's rules.
- It has its own data folder and keychain entries (`Identity::WINDOWS`), so it
  pairs on its own, apart from the Iced app.
- Both desktop apps register their AppUserModelID and `domovoi://` through the
  Registry API, not `reg.exe`. The Iced app takes `domovoi://` only where the
  installer hasn't put the WinUI app (`HKCU\Software\Domovoi\Installer`, or the
  `Domovoi.Installer` mark on the scheme's key).

Milestones (`just parity --matrix` counts the `windows` column, and `just parity`
fails on a gap in it):

| | What | State |
| --- | --- | --- |
| M1 | Pairing (link, or URL and code), reconnecting, the thread list (by status or project), the transcript (markdown, commands, edits, tool calls), toasts | done |
| M2 | Composer (send, stop), approvals, notifications and their buttons | done: the model list and its options, effort, plan mode, permissions, pictures, the `@`, `/` and `$` lists, the queue, question answers, secrets |
| M3 | Terminal panel, settings, command palette | done: the panel beside the thread (diff, artifacts, terminal, files, Git), every Settings section with the scheduled task editor and the companion, the palette |
| M4 | Pull requests, usage, agent sessions, folder browser, diagrams and images; parity enforced | done: `windows` is out of `WIP_CLIENTS`, and `tools/parity-allowlist.toml` lists what it leaves out on purpose (pushes and replies from a notification, as the Iced app) |

`just parity --matrix` counted 1057 of 1071 for `windows` on 2026-11-26 (308 at
M1). The other 14 are in the allowlist: 6 that no client uses, and 8 the Iced
app leaves out too. See [docs/windows-hardening.md](windows-hardening.md) for
the remaining hardening plan.

### Server and platform internals on Windows

- **Ending a child**: each agent CLI runs in a job object. Stopping it, or the
  server exiting however it exits, ends the whole process tree (a `.cmd` shim's
  `node` too). Child processes open no console window. Pi and OpenCode are asked
  first with Ctrl+Break (Node's `SIGBREAK`, the counterpart of Unix `SIGTERM`),
  and their tree ends after the same grace (1 s, 3 s). That needs the server to
  have a console, hidden or not, for them to share; `domovoi-server-launcher.exe`
  gives it one.
- **ConPTY**: ConPTY's first cursor query is answered by the server, since the
  clients' emulators don't answer it.
- **Paths**: `\\?\C:\…` paths (from `canonicalize`) are normalized before
  reaching `cmd.exe`, git, or the folder browser. The client core compares a
  Windows server's paths case-insensitively across `/` and `\`. The server and
  launcher declare `longPathAware` for paths over 260 characters.
- **Secrets and links pipe**: secret files are created with an owner-only ACL
  from creation (`CreateFileW`, counterpart of `chmod 600`). A second launch
  hands a `domovoi://` link to the running app through an owner-only named pipe
  (remote clients refused).
- **Scheduled task status**: `service status` reads the task's definition and
  numeric last result code from `schtasks` XML rather than its localized status
  column.
- **Antigravity sign-in tests**: the two `provider_turns` tests for
  `open`/`xdg-open` sign-in link interception are ignored on Windows because
  Windows opens URLs through the shell without PATH shims.
- **Dev server discovery & skills**: local dev servers for preview are listed
  from the IP Helper table; Antigravity skills link into its profile using a
  junction where a symlink requires Developer Mode; npm-installed CLIs update
  through their `.cmd` shims.
