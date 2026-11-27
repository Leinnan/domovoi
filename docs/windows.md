# Windows

Domovoi runs on Windows 10 1809 and later (x64), at the same level as macOS and
Linux:

- **The server** (`domovoi-server.exe`) runs agents (Claude Code, Codex,
  OpenCode, pi, Antigravity, and ACP agents), serves clients over the LAN and
  iroh, and has a system tray icon. It can start at sign-in or run headless as
  a scheduled task.
- **Domovoi** (`Domovoi.exe`) is the native Windows (WinUI 3) desktop app and
  the main client on Windows.
- **Domovoi Desktop** (`domovoi-desktop.exe`) is the cross-platform desktop app
  (the same one as on Linux and macOS).
- **The installers** are two per-user MSIs: the full installer (apps and
  server), or the server alone.

For building, testing, and packaging on Windows from source, see
[development.md](development.md#windows-prerequisites).

## Installing and upgrading

Everything installs into `%LOCALAPPDATA%\Programs\Domovoi`, with registry entries
under `HKCU`. No administrator privileges are needed, and the Visual C++
redistributable is not required.

### What the full MSI installs

| Feature | Default | What |
| --- | --- | --- |
| Domovoi | on | `Domovoi.exe` and its runtime; a Start-menu shortcut (toasts and taskbar button); `domovoi://` links |
| Domovoi Desktop | off | `domovoi-desktop.exe` and its Start-menu shortcut |
| Domovoi Server | on | `domovoi-server.exe` and `domovoi-server-launcher.exe`; the folder on your `PATH`; a "Domovoi Server" shortcut (the server with its tray icon) |
| Start at sign-in | off | the `Run` value `domovoi-server autostart on` writes |

`domovoi://` links open `Domovoi.exe`. A link opened from outside an app
connects nothing by itself: the app asks first
([remote-access.md](remote-access.md#links-ask-first), "Links ask first").

Before installing, the MSI stops Domovoi programs running from its folder. On
uninstall it removes:

- what the server set up itself (`service uninstall`, `autostart off`);
- what the apps registered (the `domovoi://` scheme, their app IDs);
- its own files, shortcuts, and `PATH` entry.

It leaves your data: the apps' folders (`%APPDATA%\domovoi-windows`,
`%APPDATA%\domovoi-desktop`), their Credential Manager / keychain entries, and
the server's home (`~\.domovoi`). Delete them by hand for a clean slate.

### Upgrades

A newer MSI replaces the installed one and keeps your chosen features, "Start
at sign-in" included. An older MSI is refused. `msiexec /fa` repairs an
installation. Only one of the two products is installed at a time, since both
put the server on `PATH`: installing the full MSI replaces the server-only one
(keeping "Start at sign-in"), and the server-only MSI is refused alongside the
full one.

## The server on Windows

- **The launcher.** `domovoi-server.exe` is a console program, so its
  subcommands print in the terminal you run them from.
  `domovoi-server-launcher.exe`, beside it, starts the server with no console
  window. The Start-menu shortcut, sign-in entry, and scheduled task use the
  launcher. It appends the server's stderr, each start, and each exit code to
  `%LOCALAPPDATA%\Domovoi\logs\launcher.log`.
- **Start at sign-in.** `domovoi-server autostart on|off` (or **Launch at
  Login** in the tray menu) starts the server with its tray icon when you sign
  in.
- **Headless service.** `domovoi-server service install|uninstall|status` is the
  Windows counterpart of the Linux `systemd` unit: a scheduled task, "Domovoi
  Server", that runs at sign-in with no time limit. It automatically restarts
  the server after a failure (5 s at first, doubling up to 5 minutes, resetting
  to 5 s after 10 minutes of running). A clean stop (`domovoi-server stop`)
  stays stopped, and ending the task stops the server with it. To start before
  anyone signs in, enable "Run whether user is logged on or not" in Task
  Scheduler and store your account password there.
- **Finding agent CLIs.** The server finds npm-installed shims (`claude.cmd`,
  `codex.cmd`) as well as native executables, reads your current registry
  `PATH` (so CLIs installed after you signed in are found), and checks
  `%APPDATA%\npm`, WinGet links, Scoop shims, `~\.local\bin`, Volta, pnpm, and
  bun.
- **Terminals** start PowerShell 7 (`pwsh`), then Windows PowerShell, then
  `%COMSPEC%`. Set `DOMOVOI_SHELL` to pick another shell.
- **Private files and home folder.** At each start the server restricts
  `userdata`, `remote`, and `local` under its home folder (plus secret/key
  files and `remote.json`) to your account only. The home folder itself keeps
  its inherited ACL so `worktrees` and `projects` are not affected. Run
  `domovoi-server doctor` to check file privacy.
- **Same-machine sign-in.** Desktop apps running on the same computer sign in
  without a pairing code
  ([remote-access.md](remote-access.md#the-app-on-the-same-machine), "The app on
  the same machine"). An app run elevated (as Administrator) cannot use
  same-machine sign-in and pairs with a code instead.
- **Managed policy.** Administrator policy lives at
  `Program Files\Domovoi\policy.json` (not `ProgramData`).
- **Also on Windows:**
  - Keeps the computer awake while agents work.
  - "Open in Explorer" selects the file in File Explorer.
  - Local dev servers are detected automatically for the preview panel and can
    be stopped from the app.
  - Updating npm-installed agent CLIs works through their `.cmd` shims.

## Desktop apps on Windows

Both `Domovoi.exe` (WinUI 3) and `Domovoi Desktop` (Iced) pair independently and
keep separate settings and logs (`%APPDATA%\domovoi-windows\logs` and
`%APPDATA%\domovoi-desktop\logs`, rotated daily, one week kept, with crash
backtraces in `panic.log`).

Differences in `Domovoi.exe` compared to `Domovoi Desktop`:

- **Artifacts.** Runnable web pages (HTML, React, hosted previews) open in your
  browser; source code, SVG, and Mermaid diagrams render in the side panel.
- **The companion** displays a static illustration per mood rather than frame-by-frame
  animation.
- **The terminal** renders as lines of text. `Ctrl+Shift+C` copies, `Ctrl+V`
  pastes, and `Ctrl+<letter>` goes to the shell.
- **Menus and layout.** Thread and project menus open from the button beside
  each item rather than right-click, and the sidebar and side panel use fixed
  widths.
- **Long threads** render the latest 60 entries first, loading earlier entries on
  request.

## Known limits

See [docs/windows-hardening.md](windows-hardening.md) for the tracking plan.

- **Antigravity sign-in.** On macOS and Linux, the server intercepts the Google
  sign-in link so you can complete it on another device. On Windows, the sign-in
  URL opens directly in the browser on the server machine.
- **Firewall.** Starting the server with `--host 0.0.0.0` (or another
  non-loopback address) prompts Windows Defender Firewall. The per-user MSI
  cannot add firewall rules automatically: allow it at the prompt or add a rule
  as an administrator. Remote access over iroh needs no inbound firewall rule.
- **Stopping a local dev server** ends its process tree immediately rather than
  sending a graceful shutdown signal.
- **Windows Registry policy.** Only `policy.json` is read, not
  `HKLM\Software\Policies`.
- **arm64.** Only x64 Windows builds are currently provided.
