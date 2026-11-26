# Windows

Domovoi runs on Windows 10 1809 and later (x64), at the same level as macOS
and Linux:

- **The server** (`domovoi-server.exe`) runs agents (Claude Code, Codex,
  OpenCode, Pi, Antigravity, ACP agents), serves the clients over the LAN and
  iroh, and has its tray icon. It starts at sign-in through a Run value, or
  headless as a scheduled task.
- **Domovoi** (`Domovoi.exe`) is the WinUI 3 app, built in Rust on
  `windows-reactor` like lwa_fm's and wihaister's Windows shells. It is the
  main desktop client here, and in progress (see its milestones below).
- **Domovoi Desktop** (`domovoi-desktop.exe`) is the Iced app, the same one
  as on Linux and macOS, with every feature of the core.
- **The installers** are two per-user MSIs: everything, or the server alone.

`just windows-doctor` checks the tools. `just --list` names every recipe; the
Windows ones start with `windows-`.

## Prerequisites

| Tool | Why | Get it |
|---|---|---|
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

## Build, run and test

```powershell
just windows-run                 # the WinUI app (debug)
just desktop                     # the Iced app
just server                      # the server in this terminal; `just tray` with its icon
just pair                        # a pairing link and code for a running server
just windows-smoke               # the WinUI app pairs with `just server` and must connect
just windows-check               # the workspace builds with no warning; the lint gate
just windows-test                # unit tests: WinUI app, host, launcher, Iced app, server
just windows-test-server         # the server's integration tests that pass on Windows
```

The test recipes set `core.autocrlf=false` for the git they run. Git for
Windows turns line-ending conversion on for every repository, and the tests'
own repositories expect files written byte for byte.

## Installers

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
   needs installing. They go to `target\windows-package`, a target folder of
   its own.
2. Checks no program needs `VCRUNTIME140.dll`, then stages the programs and
   the WinUI app's runtime.
3. Signs the programs and the MSIs when `DOMOVOI_SIGN_CERT_THUMBPRINT` names a
   certificate in the user's store. `DOMOVOI_SIGN_TIMESTAMP_URL` adds a
   timestamp.
4. Builds each MSI with the pinned WiX (6.0.2, with its UI and util
   extensions) and validates it.
5. Unpacks each MSI as an administrator would, starts the unpacked server
   (`--version`), and checks the unpacked WinUI app opens its window.

`DOMOVOI_RELEASE_VERSION` overrides the version, which otherwise comes from
`Cargo.toml`. The CI workflow `windows-installers.yml` runs on tags and by
hand. It signs when the `WINDOWS_CERT_PFX` (base64) and `WINDOWS_CERT_PASSWORD`
secrets are set.

### What the full MSI installs

Everything goes into `%LOCALAPPDATA%\Programs\Domovoi`, and every registry
value under `HKCU`. No administrator is needed.

| Feature | Default | What |
|---|---|---|
| Domovoi | on | `Domovoi.exe` and its runtime; a Start-menu shortcut with the app id `Domovoi.Client` (its toasts and taskbar button); `domovoi://` links |
| Domovoi Desktop | off | `domovoi-desktop.exe` and its shortcut (`Domovoi.Desktop`) |
| Domovoi Server | on | `domovoi-server.exe` and `domovoi-server-launcher.exe`; the folder on your PATH; a "Domovoi Server" shortcut (the server with its tray icon) |
| Start at sign-in | off | the `Run` value `domovoi-server autostart on` writes |

`domovoi://` links open the WinUI app. The MSI registers the scheme, and a
release build of the WinUI app points it at itself too, so a copy run without
the installer handles links as well. The Iced app takes the scheme only where
the installer hasn't put the WinUI app (`HKCU\Software\Domovoi\Installer`, or
the `Domovoi.Installer` mark on the scheme's key). A link opened this way
connects nothing by itself: the app asks first
([remote-access.md](remote-access.md), "Links ask first").

When checking this from a terminal inside the Claude desktop app, look at
the registry from outside it, for example through a scheduled task. That app
is packaged, so what its child processes write under `HKCU\Software\Classes`
goes to a private copy that only they see.

Before installing, the MSI stops the Domovoi programs running from its
folder. A server run from a checkout (`target\debug`) keeps running. On
uninstall it removes:

- what the server set up itself (`service uninstall`, `autostart off`);
- what the apps registered (the scheme, their app ids);
- its own files, shortcuts and PATH entry.

It leaves your data: the apps' folders (`%APPDATA%\domovoi-windows`,
`%APPDATA%\domovoi-desktop`), their keychain entries and the server's home
(`~\.domovoi`). Delete them by hand for a clean slate.

**Upgrades.** A newer MSI replaces the installed one and keeps its features,
"Start at sign-in" included. An older one is refused. `msiexec /fa` repairs.
Only one of the two products is installed at a time, since both put the same
server on PATH: the full MSI takes the server-only one's place (keeping
"Start at sign-in"), and the server-only MSI is refused beside the full one.
`just windows-installer-cycle` checks all of this for this user. It builds
the MSIs at the version and the one after, and refuses to run while Domovoi
is installed.

## The server on Windows

- **The launcher.** `domovoi-server.exe` is a console program, so its
  subcommands print in the terminal you run them from.
  `domovoi-server-launcher.exe`, beside it, is a windowed program: it starts
  the server with the same arguments and no console window. The Run value, the
  scheduled task and the Start-menu shortcut start the launcher. It appends
  the server's stderr (a panic before the server's own log starts, too), each
  start and each exit code to `%LOCALAPPDATA%\Domovoi\logs\launcher.log`.
- **Start at sign-in.** `domovoi-server autostart on|off` writes or removes
  `HKCU\…\CurrentVersion\Run\Domovoi Server`, which starts the server with
  its tray icon.
- **Headless.** `domovoi-server service install|uninstall|status` is the
  Windows counterpart of the Linux systemd unit: a scheduled task, "Domovoi
  Server", that runs at your sign-in with no time limit. Its command is
  `domovoi-server-launcher.exe --supervise …`, and the launcher restarts the
  server after a failure: 5 s at first, doubling to five minutes, and back to
  5 s after ten minutes of running. A clean stop (`domovoi-server stop`) stays
  stopped. Ending the task ends the server with it.
  Starting before anyone signs in needs your password stored with the task.
  Set it in Task Scheduler: "Run whether user is logged on or not".
  `service status` reads the task's state from what `schtasks` writes in
  every language (its definition and last result code), not its translated
  status column.
- **Finding the CLIs.** The server finds agent CLIs by `PATHEXT`, so it finds
  npm's `claude.cmd` and `codex.cmd`. It adds your PATH as the registry holds
  it now, so a CLI installed since you signed in is found. It also looks in
  `%APPDATA%\npm`, WinGet's links, Scoop's shims, `~\.local\bin`, Volta,
  pnpm and bun.
- **Ending a child.** Each agent CLI runs in a job object. Stopping it, or
  the server exiting however it exits, ends the whole process tree (a
  `.cmd` shim's `node` too). Child processes open no console window.
  Pi and OpenCode are asked first: they get Ctrl+Break (Node's `SIGBREAK`,
  the counterpart of the SIGTERM they get on Unix), and their tree ends after
  the same grace (1 s, 3 s). That needs the server to have a console, hidden
  or not, for them to share; the launcher gives it one.
- **Terminals** start PowerShell 7 (`pwsh`), then Windows PowerShell, then
  `%COMSPEC%`. `DOMOVOI_SHELL` picks another. ConPTY's first cursor query is
  answered by the server, since the clients' emulators don't answer it.
- **Paths.** A `\\?\C:\…` path a client sends (what `canonicalize` makes) is
  made plain before it reaches `cmd.exe`, git or the folder browser, which
  refuse it or read it as an ssh `host:path`. The client core compares a
  Windows server's paths without regard to case or separator, and takes
  names and parents at `\` as well as `/`. The server and its launcher
  declare `longPathAware`, for paths over 260 characters where Windows has
  long paths on.
- **Secrets.** Token files, the iroh key and the push keys are created with
  an ACL of their own that grants only you, from the start
  (`CreateFileW` with that descriptor): the Windows counterpart of
  `chmod 600`. `domovoi-server doctor` warns when the iroh key or any other
  secret file can be read by other accounts, and the server tightens a key
  made before this change.
- **The apps' links pipe.** A second launch hands a `domovoi://` link to the
  running app through a named pipe that only you can open (the Unix socket's
  mode 0600); remote clients are refused.
- **The home folder.** At each start the server gives `userdata`, `remote`
  and `local` under its home an ACL for your account only (the database,
  logs, keys and the sign-in files of apps on this machine), and
  `remote.json` one of its own. The home folder itself keeps the ACL it
  inherits: a protected one there would spread to `worktrees` and
  `projects`. `domovoi-server doctor` lists what is not private.
- **The apps on the server's machine** sign in with no pairing code
  ([remote-access.md](remote-access.md), "The app on the same machine"):
  the server writes `%USERPROFILE%\.domovoi\local\server.json` and the app
  proves itself by writing a request file beside it. The app does not check
  who owns those files (the folder's ACL is what keeps other accounts out),
  and the server refuses a request file your account does not own, so an
  app run elevated, whose files Administrators own, pairs with a code.
- **Managed policy.** The policy file is `policy.json` in `Program
  Files\Domovoi`, with the folder read from the registry. It is not in
  ProgramData, where any user could create the folder first.
- **Also on Windows:**
  - Keep-awake while agents work (`SetThreadExecutionState`).
  - Open in Explorer, with the file selected.
  - Local dev servers for the preview panel (listed from the IP Helper
    table), and stopping them, which ends the listener and what it started.
  - Antigravity's skills, linked into its profile with a junction where a
    symlink needs Developer Mode.
  - Updating npm-installed CLIs through their `.cmd` shims.

## The WinUI app

`apps/domovoi-windows` is a pure-Rust WinUI 3 app on the `windows-reactor`
fork that lwa_fm builds on (`Cargo.toml`, `[workspace.dependencies]`). The
fork pins one rev for every windows-rs crate.

- The client core (`domovoi_core`) is linked directly. Its effects run in the
  host it shares with the Iced app (`crates/domovoi-desktop-host`): HTTP,
  WebSocket, the iroh tunnel, the keychain store and notifications.
- The app is unpackaged and self-contained. `AGENTS.md` beside its crate maps
  the modules and gives the reactor's rules.
- It has its own data folder and keychain entries (`Identity::WINDOWS`), so it
  pairs on its own, apart from the Iced app.
- **Logs.** Both desktop apps write a log a day to `logs` in their data
  folder (`%APPDATA%\domovoi-windows\logs`, `%APPDATA%\domovoi-desktop\logs`),
  a week kept, since a windowed program's stderr goes nowhere. A panic also
  goes to `panic.log` there at once, with its backtrace: a panic inside a
  WinUI callback ends the process before the log's writer runs.
- **Registry.** The apps register their AppUserModelID and `domovoi://`
  through the Registry API, not `reg.exe`.

Milestones (`just parity --matrix` counts the `windows` column; `WIP_CLIENTS`
in `tools/parity.py` keeps its gaps out of the check until M4):

| | What | State |
|---|---|---|
| M1 | Pairing (link, or URL and code), reconnecting, the thread list (by status or project), the transcript (markdown, commands, edits, tool calls), toasts | done |
| M2 | Composer (send, stop), approvals, notifications and their buttons | send, stop and approvals done; question answers, attachments, model picker to do |
| M3 | Terminal panel, settings, command palette | to do |
| M4 | Pull requests, usage, agent sessions, folder browser, diagrams and images; parity enforced | to do |

## Known limits

`docs/windows-hardening.md` is the plan for these and the rest of what is
left, in order.

- **Antigravity sign-in.** On macOS and Linux the server intercepts the
  sign-in link through `open`/`xdg-open` shims. Windows opens it through the
  shell, which no PATH entry stands in front of, so the browser opens on the
  server's machine. The two `provider_turns` tests of that interception are
  ignored on Windows for this reason.
- **Firewall.** A server started with `--host 0.0.0.0` (or another address
  than loopback) makes Windows ask whether to let it through the firewall.
  The per-user MSI can't add a rule: allow it in that prompt, or add a rule
  as an administrator. iroh (remote access) needs no inbound rule.
- **Stopping a local dev server** ends its process tree at once. Ctrl+Break
  only reaches processes on the server's own console, and a dev server runs
  on its agent's or terminal's.
- **Windows Registry policy.** Only the policy file is read, not
  `HKLM\Software\Policies`.
- **arm64.** Only x64 is built: the Windows App SDK staging is untested on
  arm64.
- **Tools on macOS only.** `just icons export` needs Xcode's `ictool`. On
  Windows the `.ico` files come from Git LFS.
