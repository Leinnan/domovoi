# Remote access, the status bar icon and launch at login

`domovoi-server` can be reached from other devices over [iroh](https://iroh.computer):
dial-by-key QUIC with hole punching and an encrypted relay fallback. No account,
no port forwarding. Every Domovoi app uses it.

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

In the apps, the pairing URL (`http://localhost:13773/pair#token=…`) can be
pasted whole wherever a code or a link is asked for: where the code goes it
gives the code, and the address too when none was typed.

A server is saved in an app once the app has signed in to it. A wrong code, an
address nothing answers on or a link you decline leaves the list of saved
servers as it was.

### Links ask first

A `domovoi://connect` link you paste, type or scan inside an app connects at
once: you put it there. A link the system hands to the app (clicked on a web
page, in a mail or in another app) connects nothing by itself. The app shows
what the link is for and waits:

- the start of the server's key, which a link cannot fake;
- for a server saved in the app, the name you know it by. The name a link
  carries is whatever its author wrote: it is shown only for a server the app
  has not saved, labelled as the link's own, and it never replaces a saved
  name;
- **Cancel** and **Connect**.

Until you answer, the app stays connected to the server it is using. A link
for a saved server changes nothing saved (its address included) until that
server has accepted this device through it.

### Forgetting a server

**Forget** removes the server from the app with what the app kept for it: its
sign-in, drafts and offline copy. What happens to the sign-in on the server:

- the server in use is told first, and ends the sign-in (pushes stop too);
- another server saved by address is asked to end it with that server's own
  token, and the app says whether it could;
- another *remote* server is not dialled just for that: the app says the
  sign-in stays until it expires (30 days unused) or you remove the device on
  that server (Settings → Access, or `domovoi-server remote revoke`).

## The app on the same machine

A desktop app on the machine the server runs on signs in with no pairing code.

While it listens on a loopback address, the server keeps
`<home>/local/server.json`: where it listens and which server it is. No secret
is in it. The app (the Mac app, the Iced app, the Windows app) finds the file
in the default home (`~/.domovoi`), and proves it runs as your user by
*writing* a file: `<home>/local/requests/<id>`, with a random secret in it.
It posts the id to `POST /api/domovoi/local/session`; the server reads the
file once, deletes it, and answers with a session and the secret it read.

- The server learns that the app can write its home, which only your user
  can. `domovoi-server pair` needs the same, so nothing is given away.
- The app learns that whoever answered could read the home. Another program
  that has taken the server's port cannot return the secret, and the app
  drops its answer, token and all.
- Nothing reusable is on disk: a request works once, for a minute.

The route answers only a loopback TCP connection that carries no `Origin`
header: never an iroh peer, never a web page.

When it happens:

- **By itself, once per install**: at a first launch with no saved servers.
  The app notes that (`localAuto` in its store), and forgetting a server sets
  the note too, so a server you forgot is not joined again at the next launch.
- **When you ask**: with other servers saved, the connect screen offers
  "Server on this computer" with a **Connect** button.
- **Silently, for its saved entry**: when the entry has lost its sign-in
  (the server answered 401, or the token is gone), the app asks for a new one,
  once; if that does not connect either, it shows the pairing form.

Limits:

- Only a server in the default home, listening on loopback, is found. With
  another `--home-dir`, point the app at the file:
  `DOMOVOI_LOCAL_SERVER=/path/to/home/local/server.json` (`off` turns the
  search off). `--host 0.0.0.0` publishes nothing; pair with a code.
- Signing a same-machine app out (`remote revoke`, Settings → Access) lasts
  until the app signs in again, which it does by itself. To keep apps on the
  machine out, start the server with `--no-local-sign-in`.
- A managed policy whose `limits.clients` leaves out `desktop` turns it off.
- A program running as your user can already read the server's database and
  mint a code; this adds nothing for it. It is no barrier between programs of
  the same user, only between users.
- On Windows the app does not check who owns the files; the `local` folder
  has an ACL for your account only. An app running elevated writes request
  files owned by Administrators, which the server refuses: such an app pairs
  with a code.

`domovoi-server doctor` reports the descriptor (`local-sign-in`).

## What the server answers to over HTTP

A web page can reach a server on `127.0.0.1` through a DNS name of its own
that it points there (DNS rebinding); the browser then treats the server as
the page's own site. Two checks on plain TCP close that. Requests over iroh
are not checked this way: there is no browser and no DNS there.

**The `Host` it is addressed by.** Answered: IP addresses, `localhost` and
`*.localhost`, this machine's own names (its host name, short and full, and
`<name>.local`), names under `.ts.net`, `.lan`, `.internal` and `.home.arpa`,
and the hosts of `--host` and `--hooks-url`. Any other name gets
403 (`HostNotAllowedError`), with the flag to use in its message:

```sh
domovoi-server --allowed-host domovoi.example.com    # a reverse proxy's name
domovoi-server --allowed-host .corp.example          # every name under it
domovoi-server --allowed-host '*'                    # any name (no check)
```

`/health`, `/ready` and webhook URLs (`/api/hooks/…`) answer under any name.

> **Upgrading.** A server reached through a reverse proxy or a DNS name of
> your own answers 403 until it is started with `--allowed-host <name>`. The
> apps show the server's message. `autostart on` and `service install` carry
> `--allowed-host` and `--no-local-sign-in` into the login item,
> the unit and the scheduled task: run them again with the new flags.

**The `Origin` of a request that changes something.** A request other than
GET or HEAD with an `Origin` header is answered only when the origin is the
server's own (the `Host` it was sent to). Programs send no `Origin`; a page
of another site gets 403. A session is presented only as a bearer token:
there are no cookies, so a page has nothing of a signed-in app's to send.

**Wrong pairing codes.** Over plain TCP an address gets ten failed pairing
attempts a minute (a hundred for all addresses together); after that a wrong
code is answered 429. A code that is still valid always pairs, so guessing
from this machine cannot lock out `domovoi-server pair`, the command line or
an app here; the lookups of codes past the budget are capped as well (120 a
minute per address). What makes guessing hopeless is the code itself: 60
random bits, used once. Over iroh the limit is per device key, as before.

## Who can read the home directory

The home holds every thread, the session and code hashes, and the server's
keys. At each start the server closes it to other accounts on the machine:

- `userdata` (the database, logs, attachments), `remote` and `local` are mode
  0700; the database with its `-wal` and `-shm`, `remote.json`, the saved
  ticket and the attachment signing key are 0600. New files in those folders
  are out of reach whatever their own mode.
- The home itself becomes 0700 if group or others could read or write it. A
  home they can only pass through is left alone: `chmod 711 ~/.domovoi` is how
  to keep worktrees under it reachable for an agent or server of another
  account, while `userdata` stays closed.
- A home the server's user does not own, or one that is your own home folder,
  is left as it is, with a warning in the log.
- On Windows `userdata`, `remote` and `local` get an ACL for your account
  only. The home folder itself keeps its inherited ACL, which would otherwise
  spread to `worktrees` and `projects`.

`domovoi-server doctor` warns about a readable home (`home`) and lists what is
not private (`private`).

## The status bar icon

```sh
domovoi-server --tray
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

On macOS, launch **Domovoi Server.app** (no Dock icon) from `/Applications` to
start the server with its status bar icon. On Linux, without a desktop session
the server runs without an icon. For building the tray app or headless binaries
from source, see [development.md](development.md).

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
current home directory, port, `--allowed-host` and
`--no-local-sign-in` when you sign in: a LaunchAgent on macOS
(`~/Library/LaunchAgents/com.crayen.domovoi.server.plist`), a `Run` value on Windows, an
XDG autostart entry on Linux. An entry left by an older copy of the executable
is rewritten at start.

macOS does not let programs started by launchd read `~/Documents`,
`~/Desktop`, `~/Downloads` or iCloud Drive without asking, and nobody is there
to answer. So for a bare binary `autostart on` refuses an executable or home
directory in those folders: use **Domovoi Server.app**, or copy the binary to
`~/.local/bin`.

## Logs and shutdown

Logs go to stderr and to `<home>/userdata/logs/server.<date>.log` (a week is
kept). Pairing codes are never written there. Ctrl-C, SIGTERM, SIGHUP, the
menu's Quit, `domovoi-server stop` and (on Windows) the session ending all shut the
server down the same way: open sockets get a Close frame, terminals stop, the
iroh endpoint closes, and its saved ticket and `local/server.json` are removed.

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

## Push notifications (Android)

Android needs no keys on the server. Install a UnifiedPush distributor on the
phone (ntfy, NextPush or another) and open the Domovoi app once: it registers
the distributor's endpoint with each server it connects to
(`POST /api/domovoi/push/webpush`). The server then sends "Needs your
approval", "Finished" and "Failed" as encrypted Web Push messages (RFC 8291,
signed with the server's own VAPID key, made on first use at
`<home>/webpush-vapid.p8`) whenever the app has no connection open. Approvals
can be allowed or declined from the notification. Without a distributor the app
keeps its local notifications while it runs.

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

For cross-building the headless Linux binary or deploying over SSH with `just`,
see [development.md](development.md).
