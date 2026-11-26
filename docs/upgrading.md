# Upgrading to the Domovoi-only server

Domovoi began as a Rust rewrite of T3 Code and kept its wire protocol, its
API and its names. It no longer does. This page lists what changes for an
existing installation, in the order you meet it.

## Update the server and the apps together

The server and the apps speak protocol version 4
(`domovoi_model::PROTOCOL_VERSION`). An app and a server of different
versions do not work together: the app says which of the two to update and
keeps asking, so it connects by itself once the other side is updated.

An app from before the versions were compared (it shows no such message of
its own) is told the same in the one place it shows what a server says: an
error toast.

## Your data is migrated at the first start

The first start of the new server changes the database
(`<home>/userdata/domovoi.sqlite`):

1. The whole database is copied to `<home>/userdata/backups/` first.
2. The event log is cut down to its newest 20,000 events. Every older event
   was already applied to the threads it changed; the log is read only by an
   app that reconnects and asks for what it missed.
3. The execution node recorded beside every turn item is deleted. Nothing
   read them.
4. The file is compacted.

On a home of 1,027 threads this took under six seconds and the database went
from 1,125 MB to 388 MB ([performance.md](performance.md) has the numbers).
`domovoi-server doctor --records` reads every stored record and counts those
that do not decode; it is the check to run before and after.

A server older than the database refuses to start on it, and says so. To go
back, stop the server and put the backup in the database's place.

Sessions of the web client (signed in with a cookie) are deleted. Paired
devices stay paired.

## The web client is gone

- `/api/auth/browser-session` and cookie sessions are removed. A session is
  presented as a bearer token only.
- `--web-url` is still read, so that a service installed with it starts, and
  does nothing. Run `domovoi-server autostart on` or `service install` again
  to drop it from the login item or the unit.
- A request that changes something is answered only when it carries no
  `Origin` or the server's own.

## Names that changed

Where something outside the server may still hold the old name, both are
read for now.

| Was | Is | The old name |
|---|---|---|
| MCP server `t3` (Claude tools `mcp__t3__…`) | `domovoi` (`mcp__domovoi__…`) | not served; a call under an old tool name still runs |
| Tools `t3_thread_read`, `t3_artifact_create`, … (35 of them) | `thread_read`, `artifact_create`, … | a call under the old name still runs |
| What a thread was allowed for good (`Allow for Thread`) | kept under the tools' new names | rewritten at the first start |
| `t3.json` in a project's root | `domovoi.json` | read where there is no `domovoi.json`; never renamed |
| `T3CODE_PROJECT_ROOT`, `T3CODE_WORKTREE_PATH`, `T3CODE_THREAD_ID` | `DOMOVOI_PROJECT_ROOT`, `DOMOVOI_WORKTREE_PATH`, `DOMOVOI_THREAD_ID` | set beside the new ones for one release |
| `T3CODE_SERVER_BROWSER_SANDBOX` | `DOMOVOI_SERVER_BROWSER_SANDBOX` | still read |
| Checkpoint refs `refs/t3/orchestration-v2/checkpoints/…`, committed by "T3 Code" | `refs/domovoi/checkpoints/…`, committed by "Domovoi" | old checkpoints keep their refs and still restore |
| Default branch prefix `t3`, new worktrees on `t3/<8 hex>` | `domovoi`, `domovoi/<8 hex>` | a prefix set in the settings wins; existing branches are not renamed |
| A fork's pull request checked out as `t3code/pr-N/…` | `domovoi/pr-N/…` | existing branches are not renamed |
| `/.well-known/t3/environment` | `/.well-known/domovoi/environment` | still answered |
| `urn:t3:params:oauth:token-type:environment-bootstrap` | `urn:domovoi:…` | still accepted |
| `x-t3-hook-outcome`, `x-t3-tunnel-*` response headers | `x-domovoi-hook-outcome`, `x-domovoi-tunnel-*` | gone |
| The server's version `0.1.9` (the T3 Code release it mirrored) | its own (`0.1.0`) | |

If a project script reads `$T3CODE_PROJECT_ROOT`, change it to
`$DOMOVOI_PROJECT_ROOT`: the old name is set for one more release.

If you set a branch name prefix in the settings, nothing changes for you.
If you did not, new branches start with `domovoi/` where they started with
`t3/`.

Kept as they are, because they match what other programs write: the
`/.t3/worktrees/` folders the session import leaves out, the `x-t3-relay-*`
headers a webhook's log redacts, and the `t3-context:` marker in a webhook's
body.

## What an agent sees

An agent resumed from before the update has the old tool names in its
transcript. Its tools are listed again under their new names when it starts,
and a call under an old name is still carried out.

The names of tools in a thread's history are not rewritten: a call is shown
as it was recorded.

## Removed with no replacement

45 RPC methods that only T3 Code's web and desktop clients called, among them
the server's self-update, npm management of the server package, the ChatGPT
sign-in stubs and the editor launcher. The apps never called them.
