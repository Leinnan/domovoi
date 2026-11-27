# Domovoi

Domovoi runs your coding agents — **Claude Code**, **Codex**, **OpenCode**, **pi**,
**Google Antigravity**, and any [ACP](https://agentclientprotocol.com) agent — on
your own computer and lets you drive them from native apps on macOS, Windows,
Linux, iPhone, iPad, and Android, at your desk or away from it.

## How it works

- **Domovoi Server** runs on the machine where your repositories and coding
  agents live.
- **Domovoi Apps** connect to your server from your desktop, phone, or tablet.
- **Private remote access**: connect from anywhere with a pairing link or QR
  code. Connections are end-to-end encrypted peer-to-peer (with an encrypted
  relay fallback), with no account, VPN, or port forwarding required.

## Supported agents and platforms

**Coding agents:** Claude Code (`claude`), Codex (`codex`), OpenCode (`opencode`),
pi (`pi`), Google Antigravity (`agy`), and any Agent Client Protocol (ACP) agent
(added by command or installed from the built-in ACP registry, including Grok,
Kiro, and fx).

| Platform | Apps | Server |
| --- | --- | --- |
| **macOS** (15+) | Native Mac app (`Domovoi.app`) and `Domovoi Desktop` | `Domovoi Server.app` (menu bar) or CLI |
| **Windows** (10 1809+, x64) | Native Windows app (`Domovoi.exe`) and `Domovoi Desktop` | Tray app, CLI, or background scheduled task |
| **Linux** | `Domovoi Desktop` | Tray app, CLI, or headless `systemd` user service |
| **iPhone & iPad** (iOS/iPadOS 26+) | Native iOS/iPadOS app (widgets, Share extension, Live Activities) | — |
| **Android** (10+) | Native Android app | — |

## What you can do

### Projects, code, and existing sessions

- **Projects and threads** for each agent, with inline diffs, checkpoints to
  rewind changes, git worktrees, custom project icons, and source control
  helpers for Git, GitHub pull requests, and GitLab merge requests (including PR
  review).
- **Your existing sessions**: folders you have worked in with Claude Code or
  Codex appear automatically as projects. New terminal sessions sync in, and
  continuing a thread in Domovoi resumes the same session.
- **Issues** from GitHub, GitLab, and Linear right beside each project, linked
  to the threads working on them.

### Stay in control from your phone or desk

- **Approvals, questions, and turn control**: approve or decline tool calls,
  answer agent questions, steer an active turn, or edit and reorder queued
  follow-ups.
- **Interactive terminals**: terminals stay live when you disconnect and recover
  their screen when catching up. Watch a terminal from another device without
  resizing the shell, use split panes and an All Servers terminal view on
  desktop, and optionally preserve the last visible screen across server
  restarts (**Save the last terminal screen** in server settings). On macOS and
  Linux, recognized Claude Code and Codex terminal prompts appear in the
  sidebar.
- **Push notifications** for "Needs your approval", "Finished", and "Failed": on
  iPhone with Live Activities tracking the active turn (using your own Apple
  push key), and on Android through any UnifiedPush distributor (no Google
  services required).

### Previews, artifacts, and automation

- **Artifacts, diagrams, and images**: versioned interactive artifacts (HTML and
  multi-file bundles, React/JSX previews, SVG, Mermaid diagrams, Markdown, and
  code) and inline image previews, including Git LFS files.
- **Live web preview**: view and interact with a headless browser running on the
  server machine (using your local Chrome, Chromium, or Edge), and open the
  server's local dev servers from your other devices through a built-in tunnel.
- **Scheduled tasks and webhooks**: run prompts on an interval, at fixed times
  of day and weekdays, once at a set time, or when a webhook URL receives a
  request (with HMAC-SHA256 signature verification, filters, deduplication, and
  coalescing).
- **Agent orchestration tools**: running agents can read and message sibling
  threads, delegate tasks to child agents across providers or worktrees, hand
  off work to git worktrees, schedule tasks, publish artifacts, link issues and
  pull requests, ask you for a secret without storing it in the conversation,
  and drive the preview browser.
- **Usage limits and recovery**: tracks rate-limit windows for Claude Code,
  Codex, and Antigravity across devices, and can snooze and resume threads once
  a limit resets.

### Private and self-hosted

- **No pairing on the same machine**: a desktop app automatically finds the
  server running beside it and signs in as your user without needing a pairing
  code.
- **One-time device pairing**: pair remote devices once with a link or QR code.
  Each device's sign-in is bound to its own key, and pairing links opened from
  outside an app always ask for confirmation before connecting.
- **Linked servers**: connect another Domovoi server (read-only or full access)
  and view its threads alongside your local ones.
- **Status bar controls & admin policy**: manage remote access, pairing, paired
  devices, and launch at login from the status bar icon; optional administrator
  policy files can lock settings or disable remote access, webhooks, specific
  providers, or the preview tunnel.

## Getting started

### Before you start

Install and sign in to at least one supported coding agent on the machine that
will run Domovoi Server:

- **Claude Code** (`claude`), **Codex** (`codex`), **OpenCode** (`opencode`
  2.0.18+), or **pi** (`pi`)
- **Google Antigravity** (`agy` 1.5+) or **ACP agents** can also be installed
  and signed in directly from **Settings > Providers** inside any Domovoi app.
  When signing in to Antigravity from another device, paste Google's redirect
  URL back into the sign-in field.

### 1. Download and install

Download the latest release for your platform from
[GitHub Releases](https://github.com/Leinnan/domovoi/releases):

- **macOS**: Open `Domovoi-<version>-macos.dmg` and drag **Domovoi.app** and
  **Domovoi Server.app** into `/Applications` (or download the `domovoi-server`
  tarball for terminal use).
- **Windows**: Run the per-user MSI installer (`Domovoi-<version>-x64.msi` for
  the apps and server, or `Domovoi-Server-<version>-x64.msi` for the server
  only). No administrator privileges are needed. See
  [docs/windows.md](docs/windows.md) for installer options and details.
- **Linux**: Unpack `domovoi-server-<version>-linux-<arch>.tar.gz` for the
  server, and/or `domovoi-desktop-<version>-linux-<arch>.tar.gz` for the desktop
  app (run `./install.sh` inside the unpacked folder to install the app icon,
  launcher entry, and `domovoi://` link handler).
- **iPhone, iPad, and Android**: Install from a release build / TestFlight when
  published, or build from source.

Want to build from source instead? See [docs/development.md](docs/development.md).

### 2. Start the server and connect locally

1. Launch **Domovoi Server** from your applications menu (or run
   `domovoi-server` / `domovoi-server --tray` in a terminal).
2. Open the Domovoi desktop app on the same machine. It finds the local server
   and signs in automatically.

### 3. Connect from your phone or another device

1. Turn on **Remote Access** from the status bar icon, or run once in a
   terminal:
   ```sh
   domovoi-server --remote
   ```
2. Choose **Show Pairing QR Code** or **Copy Pairing Link** from the status bar
   menu, or run:
   ```sh
   domovoi-server pair --remote
   ```
3. Scan the QR code or open the `domovoi://` link in the Domovoi app on your
   other device.

### 4. Run in the background (optional)

- **Start at login (desktop)**: enable **Launch at Login** in the status bar
  menu, or run:
  ```sh
  domovoi-server autostart on
  ```
- **Headless server (Linux or Windows)**: install as a `systemd` user service on
  Linux or a scheduled task on Windows:
  ```sh
  domovoi-server service install          # add --linger on Linux to start at boot
  ```
- **Check your setup**: run `domovoi-server doctor` to verify the home folder,
  database, agent CLIs, Git tools, remote access, and ports.

## Documentation

### User guides

- [Remote access, pairing, push notifications, webhooks & Linux servers](docs/remote-access.md)
- [Windows guide (installers, apps, server & known limits)](docs/windows.md)
- [Upgrading from older T3 Code-compatible versions](docs/upgrading.md)

### Developers and contributors

- [Development guide (architecture, repository layout, building, packaging & testing)](docs/development.md)
- [Verification and automated test suites](docs/verification.md)
- [Performance measurements and benchmarks](docs/performance.md)
- [TestFlight uploads](docs/testflight.md)

## License

[MIT](LICENSE). The default settings, keybindings and capabilities in
`apps/server/defaults` come from t3code (MIT License, Copyright (c)
2026 T3 Tools Inc.). See its [NOTICE](apps/server/defaults/NOTICE.md).
