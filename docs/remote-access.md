# Remote access, the status bar icon and launch at login

`domovoi-server` can be reached from other devices over [iroh](https://iroh.computer):
dial-by-key QUIC with hole punching and an encrypted relay fallback. No account,
no port forwarding. The iPhone and Mac apps, and the unmodified t3code web app
through `domovoi-server connect`, use it.

## Turning it on

```sh
domovoi-server --remote                 # n0's public relays
domovoi-server --remote --relay https://relay.example.com   # your own iroh-relay
domovoi-server --remote --relay disabled                    # local network only
```

The setting is kept in `<home>/remote.json`, so `--remote` is needed once. The
server's key is `<home>/remote/secret.key` (mode 0600); its endpoint id, which
devices dial, stays the same across restarts. On the local network only, the
UDP port is kept too (`lanPort` in `remote.json`), so saved links keep working.

## Pairing a device

A device pairs once with a one-time code and then holds a sign-in bound to its
own iroh key: copied to another device, or used over plain TCP, it is refused.

- Status bar icon: **Copy Pairing Link** or **Show Pairing QR Code** (valid ten
  minutes; the QR image is deleted after that).
- Terminal: `domovoi-server pair --remote` prints a `domovoi://connect?…` link and a QR code
  (drawn for a dark terminal; `--light` for a light one). `--ttl-minutes` sets how long the
  code lasts (up to 7 days), `--json` prints the link, code and expiry for scripts. It works
  with or without the server running: the code goes into the home directory's database.
- Mac app: Settings → Access.

On the iPhone, scan the code, open the link, or paste it. Sign-ins renew while
in use; one left unused for 30 days expires.

The pairing URL printed at start is valid for a day and works on this machine
only (plain TCP), never over iroh. `--no-startup-code` skips it.

## The status bar icon

```sh
domovoi-server --tray       # or `just tray`
```

The icon shows a prompt (`>_`): faded when remote access is off, solid when it
is on, with a dot while a remote device is connected and an exclamation mark
when remote access failed to start. The menu:

- the status ("Remote access on · 2 devices, 1 connected")
- **Remote Access** on or off
- **Copy Pairing Link**, **Show Pairing QR Code**
- **Devices** ▸ each paired device, with **Sign Out**
- **Launch at Login**
- **Advanced** ▸ version, endpoint id, logs and data folders
- **Quit Domovoi Server**

It is built with the `tray` Cargo feature (on by default). On Linux that needs
GTK 3, `libxdo` and an appindicator library to build (`libgtk-3-dev
libxdo-dev libayatana-appindicator3-dev` on Debian); without a desktop session
the server runs without an icon. `cargo build --no-default-features` leaves it
out.

On macOS, `just tray-app` builds `dist/Domovoi Server.app` (no Dock icon) to move to
/Applications. Building the Mac app builds it too: `just mac-build` puts a debug
copy beside `Domovoi.app` in `apps/domovoi-apple/build/Build/Products/Debug`, and `just
mac-package` ships an Apple silicon one in `dist/` (its own .zip, and in the .dmg
next to `Domovoi.app`). Its executable is `domovoi-server` itself, which starts with
the icon and without a pairing code when it is an app's executable. A launcher
script that `exec`s the server would not do: macOS 26 then hides the icon.

## Managing a running server from a terminal

These talk to the server that uses the same `--home-dir` and `--port`:

```sh
domovoi-server remote status          # state, relay, addresses, devices, push
domovoi-server remote on | off
domovoi-server remote devices
domovoi-server remote revoke <id>     # the first characters of an id from `devices`
domovoi-server stop                   # a clean shutdown
```

With no server running, `remote on|off` changes the setting for the next start.

## Launch at login

```sh
domovoi-server autostart on | off | status
```

or **Launch at Login** in the status bar menu.

As `Domovoi Server.app` on macOS it is a login item (System Settings › General ›
Login Items): macOS opens the app when you sign in, as a double-click would,
wherever the app is. If macOS asks for approval, System Settings opens on Login
Items; switch Domovoi Server on there. A LaunchAgent left by an older version is
turned into the login item at start.

Anywhere else it starts this executable with `--tray --no-startup-code` and the
current home directory and port when you sign in: a LaunchAgent on macOS
(`~/Library/LaunchAgents/com.crayen.domovoi.server.plist`), a `Run` value on Windows, an
XDG autostart entry on Linux. An entry left by an older copy of the executable
is rewritten at start.

macOS does not let programs started by launchd read `~/Documents`,
`~/Desktop`, `~/Downloads` or iCloud Drive without asking, and nobody is there
to answer. So for a bare binary `autostart on` refuses an executable or home
directory in those folders: use the app from `just tray-app`, or copy the
binary to `~/.local/bin`.

## Logs and shutdown

Logs go to stderr and to `<home>/userdata/logs/server.<date>.log` (a week is
kept). Pairing codes are never written there. Ctrl-C, SIGTERM, SIGHUP, the
menu's Quit, `domovoi-server stop` and (on Windows) the session ending all shut the
server down the same way: open sockets get a Close frame, terminals stop, the
iroh endpoint closes and its saved ticket is removed.

## Your Claude Code and Codex work

The server finds the folders you have worked in with Claude Code
(`~/.claude/projects`) and Codex (`~/.codex/sessions`):

- **At start, and every 10 minutes**, each folder with sessions from the last
  14 days becomes a project with its sessions from the last 30 days (up to
  100 per folder, 200 messages each). Your home folder, temporary folders,
  Downloads and `~/Library` are never projects.
- **Every minute**, sessions you start in the terminal in those folders show
  up as new threads, and new messages reach a thread while Domovoi has not run
  it. Once you continue a thread from Domovoi, the Domovoi copy is the live one.
- **A removed project stays removed**: the automatic import doesn't bring
  it back. Adding the folder again by hand undoes that.
- **Continuing an imported thread resumes the same session**: the next
  message runs `claude --resume` (or Codex's `thread/resume`) in that folder,
  with full access, as t3code does for imports.

Older folders are in the apps' import picker ("Import from Claude Code" in
the iPhone app's + menu, or the Mac app's sidebar). Imported sessions sit
under their project as "sessions" with a Claude Code or Codex badge.

`domovoi-server --no-session-import` (or `DOMOVOI_NO_SESSION_IMPORT=1`) turns off
the automatic import and the sync; the picker still works.

The apps can also add any folder on the server ("Add Folder on Server"
browses the server's folders), make a new project, or start a thread with
no project.

## Push notifications (iPhone)

With an APNs key, the server pushes "Needs your approval", "Finished" and
"Failed" to iPhones whose app is closed, with a badge counting threads that
wait for approval:

```sh
domovoi-server --apns-key AuthKey_XXXX.p8 --apns-key-id XXXX --apns-team-id YYYY [--apns-private-alerts]
```

The same key keeps the iPhone's Live Activities current while the app sleeps:
the step under way (a command, a file, a tool), the turn's to-do progress, what
an approval or a question asks, and how the turn came out (how long it took, the
start of its answer or its error). A change of status is pushed at once;
progress alone at most every 15 seconds, at a priority that does not use up
the activity's budget.

`--apns-private-alerts` keeps thread titles away from Apple, and Live
Activities then say only what kind of step runs and how far the to-do list has
got, never what a command, file, question or answer says. The same settings
can live in `<home>/push.json` (`keyPath`, `keyId`, `teamId`, `topic`,
`privateAlerts`). The topic defaults to the iPhone app's bundle id,
`com.crayen.domovoi`.

## Webhook tasks

A scheduled task can run when a URL receives a request (GitHub, CI, any
service that sends webhooks). The URL is on the server's own port, which
listens on this machine only. To take requests from the internet, serve the
webhook URLs (and nothing else) on an address your reverse proxy or tunnel
reaches, and tell the server the public origin so the apps show full URLs:

```sh
domovoi-server --hooks-listen 127.0.0.1:13780 --hooks-url https://hooks.example.com
```

Point the proxy's `https://hooks.example.com/api/hooks/` at
`http://127.0.0.1:13780/api/hooks/`. Each task's URL carries its own token;
add a signature secret in the task for senders that sign their requests.

## The iPhone app on a device

The Xcode project is generated (`just mac-generate`) and signs the iPhone app
with automatic signing for the team in the justfile (`DEVELOPMENT_TEAM=<team>`
picks another one; the bundle id is then `com.crayen.domovoi` on that team):

- `just ios-device` builds a debug app for a physical iPhone.
- `just ios-device-run <device>` also installs and launches it
  (`xcrun devicectl list devices` names the phone).
- `just ios-archive` makes a release archive in `dist/`.
- `just ios-open` opens the project in Xcode.

The app needs iOS 26 and runs on iPhone and iPad.

## A Linux server

On a Linux machine nobody signs in to, the server runs headless as a systemd
user service:

```sh
domovoi-server service install [--linger]   # write the unit, enable it, (re)start the server
domovoi-server service status
domovoi-server service uninstall
```

The unit (`~/.config/systemd/user/domovoi-server.service`) runs this
executable with the home directory, port and relays given to `install`, and
`--no-startup-code`: its output goes to the journal, where a pairing code must
not end up. It is restarted after a crash, not after `domovoi-server stop`.
Installing a new unit turns remote access on; after that the setting in
`remote.json` decides, so `remote off` stays off across restarts. With
lingering on (`--linger`, or `loginctl enable-linger`) it starts at boot,
otherwise when you sign in. The server keeps listening on 127.0.0.1 only:
phones reach it over iroh.

From the Mac, against `DOMOVOI_LINUX_HOST` (an ssh destination; the justfile
names a default):

```sh
just linux-deploy        # cross-build, copy to ~/.local/bin, service install, wait until it is up
just linux-pair          # a pairing link and QR code, drawn in this terminal (valid 10 minutes)
just linux remote status # any subcommand: remote devices, service status, stop…
just linux-logs          # journalctl --user -u domovoi-server -f
```

`just server-package-linux` makes a tarball in `dist/` instead. The Linux build
from the Mac (`tools/cross/linux.sh`) uses zig as the C toolchain
(`cargo install cargo-zigbuild`, `rustup target add x86_64-unknown-linux-gnu`),
needs glibc 2.34 or newer on the machine, and leaves out the status bar icon
(`--no-default-features`), so it needs no GTK.

## Building for other systems

- `just check-windows` cross-builds the server for Windows with zig
  (`cargo install cargo-zigbuild`, `rustup target add x86_64-pc-windows-gnu`).
- `tools/cross/linux.sh` cross-builds the headless server for Linux with zig
  (see "A Linux server").
- `just check-linux` builds and tests the server crates in Docker.
