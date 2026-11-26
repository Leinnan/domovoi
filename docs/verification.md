# Verifying Domovoi

What is checked, and how. Performance is measured the way
[performance.md](performance.md) describes.

## macOS Keychain access after an update

Use a throwaway macOS account and an iroh-paired test server. Build two different
releases with `just mac-package` and the same `DEVELOPMENT_TEAM`. Save the first
bundle before packaging the second. Check that build B satisfies build A's
designated requirement:

```sh
requirement=$(codesign -dr - ./Signed-A.app 2>&1 | sed -n 's/^designated => //p')
test -n "$requirement" && codesign --verify --deep --strict -R "=$requirement" ./Signed-B.app
```

1. In an older ad-hoc release, pair over iroh, select the server, and save a draft.
   Record the paired device in the server's Remote Access device list.
2. Replace the old app with signed build A. If Keychain prompts, first try
   **Deny** on one item and confirm the app shows **Keychain Access Needed**.
   Click **Retry** and select **Always Allow** for each item macOS asks about.
   Confirm the selected server and draft are restored and the same device reconnects.
3. Quit build A and replace it with signed build B. Confirm it restores the same
   state and reconnects without a Keychain prompt or a new paired device.
4. In a fresh test account, pair with build A, save a draft, and repeat the same
   update to build B. Confirm zero Keychain prompts during the update.

## Automated checks

- `just lfs-check`, the first step of `just check` and of the packaging recipes:
  Git LFS is installed, no LFS file in the checkout is still a pointer file, and
  `git lfs fsck --pointers` finds no file a `.gitattributes` rule puts in LFS
  committed as a plain blob (or the other way round).
- `cargo test --workspace`: unit tests and the suites below.
- `apps/server/tests/rpc_calls.rs`: runs every case in
  `apps/server/tests/cases/calls/*.json` (one file per area) against the real
  binary and requires the expected answer kind (first chunk / success / typed
  failure with a named `_tag`). The server runs with a system-only `PATH` and
  an empty agent home, so no case reaches a provider, `gh`, an editor or the
  real `~/.claude`. `DOMOVOI_CALLS=<area>` runs one file.
- `rpc::tests` (server unit tests): every method the client core calls is
  registered, and every registered method is either called by the client core
  or listed, with its feature, in `KEPT_WITHOUT_A_CALLER` (`apps/server/src/rpc/mod.rs`).
- `just rpc-groups` (ignored test `every_rpc_group_answers_as_recorded`): the
  files of `apps/server/tests/cases/groups`, each on a server of its own with
  stand-ins for `gh`, `ssh` and the provider CLIs: the methods called for each
  answer and failure they can give. Six to ten minutes;
  `DOMOVOI_TURNS_DUMP_DIR` writes the answers, to compare two builds.
- `apps/server/tests/ops.rs`: a signed-in client of another protocol version
  (`domovoi_model::PROTOCOL_VERSION`) gets a socket that only tells it which
  side to update; `apps/domovoi-core/tests/client_replay.rs` checks the app
  says so and does not connect.
- `domovoi-server doctor --records [--events]` reads every stored row the way
  the server does and counts those that do not decode. Run it on a copy of a
  real home before and after a change to how records are written.
- Remote access over iroh:
  - `crates/domovoi-remote/tests/loopback.rs` (two endpoints, no network): HTTP,
    concurrent connections, a WebSocket echo, the peer id reaching handlers, a bad
    ticket failing at start, and redialing after the server restarts. `-- --ignored`
    adds two live checks over n0: dialing by endpoint id alone, and relay fallback
    (a client with no direct paths gets HTTP and WebSocket through the relay).
  - `rpc_calls.rs` runs all its cases a second time through an iroh bridge to
    a server started with `--remote --relay disabled`. `rpc_calls.rs` waits
    for the failed turn's checkpoint, which finalization writes after marking
    the run failed; the diff cases raced it before.
  - `tests/remote_access.rs`: the pairing link and QR code, unknown peers limited to
    pairing, bearer token exchange, a ticketed RPC socket, the token refused over
    TCP and from another device key, the device in the access list, revocation
    sending its socket a Close frame, and the startup code refused over iroh.
    Also: a remote device cannot turn remote access off, change relays,
    restart it or stop the server, and a code it mints carries at most its own
    permissions; strangers guessing codes in parallel are limited per key and
    in total while paired devices are not; the `remote` and `stop` commands
    against a running server (including the LAN port surviving off and on);
    SIGTERM with a socket open shutting down cleanly, with no pairing code in
    the log file; and a device signing itself out.
  - `tests/push.rs`: against mock APNs hosts (h2c), a failed turn pushes "Failed"
    to closed apps with an ES256 provider token and a badge, skips the device
    whose app is open, and forgets a token APNs reports gone. A refused provider
    token is signed again and the push retried; a token unknown to production
    is delivered through the sandbox; a long non-ASCII thread id and title fit.
    A registered Live Activity gets `liveactivity` pushes on its own token and
    ends with its turn (priority 10, with start and end times).
  - `crates/domovoi-remote/tests/loopback.rs` also: requests fail fast while the
    server is away and the bridge reconnects once it is back; a dropped bridge
    frees its port; a stream that sends nothing is closed.
  - Unit tests: session renewal and expiry, local-only codes, ticket expiry,
    attempt limits, the APNs token's wall-clock age, push device ownership, the
    status bar menu for each state, the icon's drawing and QR images, and the
    launch-at-login entries for each system.
  - `apps/domovoi-core/tests/client_replay.rs` also: late replies from a previous
    server, a forgotten one or an abandoned attempt change nothing; attempts
    never overlap; backoff with jitter that starts over only after the server's
    welcome; a server refusing the device for good is not retried; going to the
    background mid-connect; a used code for a paired server; forgetting signs
    out first; notification taps for forgotten, duplicated or connecting
    servers; links at a cold start; renaming and reordering; a newer entry
    replacing an older one; a tunnel whose port was taken away.

## Milestone 1 acceptance (unmodified t3code web client against domovoi-server)

| Step | Result |
|---|---|
| Pair with the one-time code, boot, render | passed |
| Add a project by path | passed |
| Claude (Haiku 5.5) turn with an approved file edit | passed: edit applied, changed-files card, diff panel |
| Codex (GPT-6-Luna) turn with an approved command | passed: command ran, answer rendered |
| Terminal in the project | passed: login shell, output, resize |
| Checkpoint revert | passed: rewind to thread start; file restore refused outside a worktree with t3code's message |
| All traffic decodes with t3code's schemas | passed: 810 messages, 28 methods in the live session |

## Status by phase

- **Phase 1** (contracts, auth, boot RPCs): pairing, cookie session, WebSocket
  auth, config/lifecycle streams, provider statuses, shell stream with resume, add
  project, folder browsing, git branch/status/refs. Verified: 116 messages over 19
  methods pass `wire-check.ts`; the client renders projects and the composer.
- **Phase 2** (orchestration v2): thread launch, message dispatch with run
  queueing, visit/frecency, runtime/interaction mode, rename, archive, delete,
  settle, pin, checkpoint rollback (with t3code's worktree rule for file
  restore), thread snapshots and live event streams, thread shells in the
  sidebar stream. Provider turns fail with a rendered provider error until the
  adapters land. Verified: 187 messages over 22 methods pass `wire-check.ts`,
  including `launchThread` projections and `subscribeThread` events/snapshots.
- **Phase 3** (terminals, checkpoints): PTY terminals (open/attach/resize/write/
  clear/close, metadata stream, shutdown on exit); workspace checkpoints as hidden
  git refs captured before a thread's first run and after every run, with file
  summaries, checkpoint turn items, run links and the turn-diff RPCs; rollback
  restores files and marks later checkpoints stale. Verified: a shell in the real
  client's terminal drawer, checkpoints captured in the playground repo, a rewind
  to the thread start, and 467 messages over 26 methods passing `wire-check.ts`.
- **Phase 4** (provider adapters): Codex over `codex app-server` (shared process,
  thread start/resume, turn start with t3code's approval/sandbox policy per
  runtime mode, items, approvals, interrupt, `thread/revert`) and Claude over the
  `claude` CLI's stream-json protocol (one process per thread, streaming text and
  thinking, tool calls and results, `can_use_tool` approvals, interrupt,
  permission mode changes). Verified with real turns from the unmodified client:
  a Claude Haiku edit with an approved file change, its diff, and a Codex Luna
  command with an approved run; 810 messages over 28 methods pass `wire-check.ts`.
- **Phase 5** (verification): the golden replay above runs as a cargo test; the
  milestone 1 acceptance table records the live client results.
- **Phase 6** (Crux client core + macOS shell): `crates/domovoi-model` holds the
  projection rules shared by the server and the client; `apps/domovoi-core` is the Crux
  core (pairing over HTTP, a WebSocket capability, Effect RPC with acks, the shell
  stream, thread snapshots/events, approvals, interrupts, model choice);
  `apps/domovoi-apple` is a SwiftUI shell built with facet typegen, boltffi and XcodeGen
  (`just build` in that folder). Verified:
  - `crates/domovoi-model/tests/parity.rs`: from t3code's own thread snapshots plus the
    events it streamed, the derived thread shell matches all 53 shells the
    TypeScript server published, on 25 fields each (a broken rule fails it).
  - `apps/domovoi-core/tests/client_replay.rs`: recorded server frames drive the core to
    t3code's reported status and visible-item count, with approvals and checkpoints.
  - `DomovoiSmoke` (same `Core.swift`, real FFI, URLSession HTTP + WebSocket) pairs with a
    live domovoi-server, lists projects, threads and 22 models, and renders a Codex
    thread's transcript. The GUI app launches; its views were not driven
    automatically.
- **Auth follow-up**: pairing codes are stored (hashed, single use) in SQLite with
  the permissions they grant, so `domovoi-server pair` mints codes for a running server;
  every RPC is checked against t3code's own permission table
  (`conformance/rpc-scopes.ts` evaluates it) and denied with its
  `EnvironmentAuthorizationError`. Verified by `tests/pairing_permissions.rs` against
  the real binary.
- **Claude rewind**: each Claude turn records its last assistant message as the
  turn cursor; a rollback closes the live CLI and points the provider thread's
  conversation head at the target turn, and the next turn resumes the same session
  with `--resume-session-at` (t3code ClaudeAdapterV2). Verified live: after "secret
  word APPLE", then "now BANANA", rewinding past the second message and asking
  returned APPLE in the same CLI session; 1285 messages pass `wire-check.ts`.
- **All remaining methods**: the router now answers all 220 `WsRpcGroup` methods,
  each with t3code's permission (the full table, generated by
  `conformance/rpc-inventory.ts`, plus its payload rules). Verified by
  `rpc_calls.rs` (above) and live in the unmodified web client: `@` file mentions,
  thread search, Settings pages (a server setting round-trips into
  `settings.json`), scheduled tasks, keybindings and source control. Where t3code
  depends on infrastructure Domovoi does not have, the method answers with t3code's own
  typed error or empty state and the module doc says so: T3 Connect relay, guest
  sharing and peer links, the Expo/EAS device hub, self-update and
  desktop updates, provider CLI install/update and sign-in, GitHub App sign-in,
  usage pricing, trace files, and a server-driven preview browser.
- **Native client (Crux core + SwiftUI)**: `apps/domovoi-core` holds the client's
  logic (connection, sidebar, transcript, composer, terminals emulated with
  `vt100`, diffs, files, git, palette, settings, pull requests, usage,
  notifications); `apps/domovoi-apple` renders it. Verified:
  - `cargo test -p domovoi_core`: the recorded t3code session reaches t3code's
    shell status and visible-item count with message text; launching with
    model, effort and worktree; thread commands; mentions; terminals; reconnects.
  - `DomovoiSmoke` (same `Core.swift`, real FFI, URLSession) against a live
    domovoi-server: transcript, diff, file tree and preview, git status, a terminal
    running a command, @ mentions, find, search, palette, storage, diagnostics,
    settings, keybindings and usage all pass; with `DOMOVOI_SMOKE_TURN=1` a Claude
    Haiku turn launched from the composer completes with one reply.
  - `DOMOVOI_SNAPSHOT_DIR=… Domovoi.app` walks the app's screens and writes window
    captures (ScreenCaptureKit, own windows only), which were reviewed.
  - Not in the client yet: the preview browser tab, voice, devices, MCP app
    views, PR write actions and review guides, keybinding editing, and
    scheduled tasks other than fixed intervals.
  - The server's newer features in both apps: the context meter, to-do lists
    (Claude's TodoWrite and Codex plans), "Implement Plan" on a proposed plan,
    steering a running turn ("Send now", ⌘↩ on the Mac), the queue (send now,
    edit, move, remove, resume after a held stop), subagent rows with Stop and
    their own thread (kept out of the thread list), forks with a "Forked from"
    link and Merge Back, Regenerate Title, and no Plan toggle for OpenCode.
    `cargo test -p domovoi_core` covers the view and the commands sent, whose
    shapes follow t3code's `orchestrationV2.ts`. Checked by eye with a scripted
    `claude` stand-in: the Mac snapshots, and the iPhone simulator where
    reorder, steer (the agent got the `priority: "now"` message), opening a
    subagent's thread and forking were driven against the server. Not tried
    with real Claude or Codex turns.
- **Remote access and the iPhone app**: the server is reachable over iroh
  (`--remote`, or Settings → Access on the Mac). Each iroh bi-stream carries
  one HTTP/1.1 connection, and clients use a loopback bridge. Native clients pair
  through t3code's token exchange, and a token works only from the device key that
  paired. The server sends APNs pushes itself. Verified live:
  - `just mac-smoke-remote`: the macOS bridge dialed a `domovoi://connect` link, paired
    and passed every smoke check in 3.2 s (direct path on one machine).
  - The unmodified t3code web client, pointed at `domovoi-server connect` (bridge on
    the port its dev proxy uses), paired, ran a Claude Haiku turn under
    "Supervised", the approval was granted and the file was written. A fresh tab
    showed no console errors.
  - iPhone simulator: paired from a `domovoi://` deep link, then every screen over iroh
    (threads, transcript, changes, terminal, files, git, pull requests, usage,
    settings showing "Direct, 29 ms"). A Haiku turn needed approval, was approved
    on the phone, and wrote `calc.py`. Backgrounding and returning reconnected
    with the Keychain token. A simulated push (`simctl push`) opened its thread
    from the background.
  - Not verified here: real APNs delivery (needs an Apple Developer key and a
    signed build), QR scanning (needs a camera), cellular and carrier NAT, and
    device builds (`just ios-device`, `just ios-archive`).
- **Status bar icon, launch at login, hardening** (`docs/remote-access.md`).
  Verified live on macOS:
  - `domovoi-server --tray`: the icon appears in the menu bar. Driven through the
    menu (a `DOMOVOI_TRAY_TEST_DIR` seam writes the menu's state and takes clicks):
    remote access turned on, a pairing link copied and opened on the iPhone
    simulator, the phone listed as connected with the icon's dot, signed out
    from the menu (the phone back on pairing within seconds), and Quit
    stopping the server cleanly.
  - The updated iPhone app reconnected with its saved sign-in, showed the
    route under the title, and a Haiku turn's approval came up in the new
    approval bar and was allowed on the phone (`notes.txt` gained its line).
  - `just mac-smoke` and `just mac-smoke-remote` pass with the updated apps.
  - SIGTERM: exit 0, ticket file removed, no pairing code in the log file.
  - A LaunchAgent written by `autostart on` was loaded with `launchctl
    bootstrap` and its server answered `remote status`. A first copy whose
    binary sat in `~/Documents` hung opening it (macOS guards that folder from
    login items), which is why `autostart on` now refuses such paths.
  - Windows: `just check-windows` cross-builds and links the server and icon
    (zig, GNU target). Native Windows (docs/windows.md): the server's 555 unit
    tests pass on Windows 11 (`just windows-test`), and so do its integration
    suites except `provider_turns` and `rpc_calls` (`just windows-test-server`). Linux: `just check-linux` builds and
    passes all 173 server, transport, model and RPC tests in Docker (the schema
    checks are skipped there: no Node); the icon itself needs a desktop
    session. It found two Linux-only problems, now fixed: terminals failed
    without `$SHELL` where zsh is not installed, and the replay test did not
    reassemble frames split into parts.
  - Not done: the iPhone terminal still has no scrollback (the core keeps
    only the visible screen), and pull-to-refresh retries a dropped
    connection rather than resyncing a live one.
- **Adapter depth** (first phase of the t3code gap plan):
  - Codex server requests get their own answers: `item/tool/requestUserInput`
    becomes a question form (answers go back as `{answers: {id: {answers}}}`),
    `item/permissions/requestApproval` grants what was asked for the turn or
    session, `mcpServer/elicitation/request` fills the form's defaults (URL
    requests and unfillable forms are declined), and the legacy exec / patch
    approvals take review decisions. Anything else, or a request no turn is
    listening for, gets a JSON-RPC error instead of hanging Codex.
  - Context use: Codex `thread/tokenUsage/updated` and each Claude message's
    `usage` (window from the `contextWindow` option or the result's
    `modelUsage`) set the provider turn's `tokenUsage`; the provider thread
    keeps the latest as `contextUsage`.
  - File attachments reach the agent: each named by its stored path, text
    files up to 32 KiB inlined.
  - Plans: Claude `TodoWrite` and Codex `turn/plan/updated` become `todo_list`
    items, Claude `ExitPlanMode` (denied, so Claude stops) and Codex plan
    items become `proposed_plan` items, each with a `plan.updated` artifact.
    `message.dispatch` with `sourcePlanRef` completes an active plan and
    links the run to it. Codex web search, MCP calls, image views and
    compaction, and Claude compaction and API retries, get their own items.
  - Verified: unit tests for each reply shape and mapping;
    `provider_turns.rs` runs a scripted Claude plan turn (to-do list,
    proposed plan, context use, implementing it once). With
    `DOMOVOI_WIRE_OUT=<file>` those tests record their frames, and
    `node conformance/wire-check.ts <file>` (Node 26) decoded all 195 with
    t3code's schemas. `rpc_calls.rs` and `golden_replay.rs` pass with
    `DOMOVOI_NODE` pointing at Node 26.
  - Live (2026-11-19, see "Live turns of the real CLIs"): plans, approvals,
    context use and image attachments against real Claude and Codex. Not yet
    live: Codex question forms, permissions and elicitation requests, and
    compaction. Before that: no real Codex or Claude turn exercised these paths
    yet (needs the CLIs signed in and the unmodified web client).
- **Turn control** (second phase of the t3code gap plan):
  - `message.dispatch` honours `dispatchMode`: `steer_active` adds the
    message to a running Claude or Codex turn (Claude gets a `priority: now`
    user message, Codex `turn/steer`); the message and its `steer` turn item
    belong to that run. A turn that isn't running, or a provider that
    refuses, queues the message as a run of its own. `restart_active` stops
    the active run and puts the new message first. A message the user sends
    releases a held queue.
  - Queue commands: `queued-run.edit`, `.cancel`, `.reorder`, `.start`,
    `queue.resume` and `queued-message.promote-to-steer`; `run.interrupt`
    with `holdQueue` holds what is queued.
  - `thread.delete` interrupts the active run, cancels queued runs and
    closes the thread's terminals. Sidebar commands: `thread.tag.set`,
    `thread.pin.reorder`, `thread.active.reorder`, `thread.auto-settle` and
    `thread.auto-settle.set`.
  - Verified: orchestrator unit tests (steer and fallback, queue editing,
    held queues, restart, delete), `provider_turns.rs` steering a scripted
    Claude turn, 8 new `rpc-calls` cases, and 390 recorded frames passing
    `wire-check.ts`. Live (2026-11-19): steer, queue and stop against real
    Claude and Codex; the live run found Claude 2.1 queuing a steer as a
    command of its own (see "Live turns of the real CLIs").
- **Workspace lifecycle** (third phase of the t3code gap plan):
  - Setup scripts: a new worktree runs the project's `runOnWorktreeCreate`
    script (resolved as t3code does: per-project override, the project's
    scripts, `defaultProjectScripts`, else `t3.json` in the project root) in
    the thread terminal `setup-<id>`, with `T3CODE_PROJECT_ROOT` and
    `T3CODE_WORKTREE_PATH`. Settling a thread closes `stopOnSettle` script
    terminals and runs the `runOnSettle` script.
  - Auto-pull at startup: projects marked for it (`autoPull`,
    `defaultAutoPull`, `projectAutoPullOverrides`) fast-forward when on
    their default branch, with an upstream, no local changes and no local
    commits.
  - Storage cleanup removes thread worktrees by the `storageCleanup` rules
    (on delete, after N days inactive or settled, or unchanged once
    settled/archived), never while an agent works there, with local files
    `worktreeKeepWhen` keeps, or with commits neither pushed nor in the
    project checkout; `logsAfterDays` sets the log retention. The defaults
    remove no worktree. `worktreeOnMerge` is not applied.
  - Keep-awake: while a provider turn runs and `keepAwakeWhileAgentsWork`
    is on, the server holds `caffeinate -i -w <pid>` (macOS) or
    `systemd-inhibit` (Linux).
  - Verified: `provider_turns.rs` launches a worktree thread whose
    `t3.json` setup script writes a file with t3code's environment;
    unit tests against real git repositories for every cleanup rule and
    each auto-pull condition; script resolution. Keep-awake is unit-tested
    for its decision only. The live steer test sees the `caffeinate` held
    while real Claude and Codex work (not checked with `pmset`).
- **Text generation** (fourth phase of the t3code gap plan): `text_gen.rs`
  runs t3code's prompts through the local `claude -p` (JSON schema, no
  tools, no session) or `codex exec` (read-only, output schema), one at a
  time, with the model from `textGenerationModelSelection` (or the source
  control / review guide selection).
  - Thread titles: a launch with `generateTitle` keeps the seed title and a
    `titleRegeneration` until `thread.title.regeneration.complete` brings the
    model's title; `thread.metadata.update` with `regenerateTitle` asks again
    from the whole thread. A stale answer is ignored.
  - Branch names: a new worktree's temporary branch is renamed after the
    first message (`branchNamingMode`, `branchNamePrefix`), only while it is
    still checked out and unpushed.
  - Commits without a message, and feature branches, get a written message;
    new pull requests get a written title and body (with the repository's
    template), falling back to `gh pr create --fill`.
  - Review guides: `generateReviewGuide` checks the head out in a scratch
    worktree and runs a read-only investigation; `reviewGuide` returns the
    stored guide and the run (running or failed).
  - Verified: `provider_turns.rs` with a scripted `claude -p`: a launch gets
    the generated title and branch `t3/add-search` in its worktree, and a
    commit without a message is committed with the written one; 342 frames
    pass `wire-check.ts`. Unit tests for prompts, answer parsing, title and
    branch cleanup, stale title answers, branch renaming and guide stops.
    Live (2026-11-19): titles, branch names and commit messages from real
    Claude Haiku and Codex. Review guides were not run end to end.

- **Fork and context handoff** (fifth phase of the t3code gap plan):
  - `thread.fork` creates the fork (lineage, `forkedFrom`, the source's
    settings and workspace) and a pending context transfer at the source
    run, with the provider's native thread and turn refs. The fork's first
    message takes it up: on the same provider it forks natively (Claude
    `--resume=<source> --fork-session --resume-session-at=<last reply>`,
    Codex `thread/fork` with `lastTurnId`); on another it gets a portable
    handoff, the source conversation as text ahead of the message, with a
    `context-handoff` and a `handoff` turn item.
  - `provider.switch`, and any message whose provider differs from the
    active provider thread's, hands the thread's earlier turns over the
    same way. `thread.merge_back` sends a fork's own turns back into its
    parent's next message.
  - Verified: unit tests for the source point, transcript, native and
    portable forks, provider switch and merge-back; `provider_turns.rs`
    forks a scripted Claude session (checked flags) and hands a fork to
    Codex as text; 88 frames pass `wire-check.ts`. Live (2026-11-19): a
    Claude fork (`--session-id` with `--fork-session`) and a Codex fork
    (`thread/fork`) remember what their source was told; a thread switched
    from Claude to Codex keeps its conversation.
- **Durability** (sixth phase of the t3code gap plan):
  - Effects that must happen even across a restart (a rollback, a title,
    a branch name, a settle script) are kept in `effect_outbox` from the
    moment they are queued until the worker has run them, and are queued
    again at startup. Effects bound to a live provider process (respond,
    interrupt, steer) are not. Unlike t3code, the outbox row is written
    right after the command's events rather than in the same transaction.
  - The maintenance sweep (every 5 minutes) drops the events and entities
    of threads deleted more than 30 days ago, keeping a tombstone record,
    and command receipts older than 7 days (receipts now carry a time).
  - Restart continuation: a deliberate stop (SIGTERM, Ctrl-C, the status
    bar) remembers the turns it cuts off; at the next start, with
    `continueThreadsAfterServerUpdate` on (the default), each gets a system
    message (`restartContinuationOfRunId`) asking the agent to continue. A
    crash continues nothing.
  - Verified: unit tests for the outbox (durable effects only, replay) and
    pruning; `provider_turns.rs` stops a server with SIGTERM mid-turn and
    the next start continues the turn; 84 frames pass `wire-check.ts`.
- **Subagents as child work** (seventh phase of the t3code gap plan):
  - A Claude `Task`/`Agent` call, or a Codex `spawnAgent` collab call,
    becomes a `subagent` entity, node and turn item in the parent run, and a
    child thread (lineage `subagent`, `forkedFrom` its node) holding the
    prompt, the subagent's own messages and tool calls, and its result.
    Claude's nested stream (`parent_tool_use_id`) no longer shows in the
    parent transcript. Claude `task_progress` sets the progress text;
    `task_notification` and the tool result end it; Codex `agentsStates`
    end its agents. Subagents still running when the turn ends end with it.
  - `subagent.stop` stops one Claude subagent with the `stop_task` control
    request (by the task id `task_started` reported); Codex agents can only
    be stopped with the turn. `getNativeChildWork` still answers empty, and
    background tasks are not listed in `pendingBackgroundTasks`.
  - Verified: `provider_turns.rs` runs a scripted Claude subagent (child
    thread contents, the parent left clean, progress, stop through
    `stop_task`); 103 frames pass `wire-check.ts`. The Codex collab mapping
    is not exercised by a test. Live (2026-11-19): a real Claude `Task`
    subagent gets its child thread; Codex 0.170 spawned none (its
    multi-agent feature is off by default).
- **Agent MCP server** (eighth phase of the t3code gap plan): `/mcp`
  answers MCP over Streamable HTTP (JSON-RPC POSTs answered as JSON) for
  the agents Domovoi runs, from this machine only, each session with its own
  bearer token naming its thread. Claude gets it through `--mcp-config`
  (read-only tools pre-allowed with `--allowedTools`), Codex through
  `mcp_servers.t3` in its thread config. `DOMOVOI_NO_AGENT_TOOLS=1` turns it off.
  - Tools, all within the caller's project: `t3_thread_list`,
    `t3_thread_read`, `t3_thread_send`, `t3_thread_wait`, `delegate_task`
    (a child thread with `delegatedFrom`, sharing the folder or in a new
    worktree), `task_status`, `task_cancel`, `schedule_task`,
    `t3_project_info`. When a delegated task's work settles, the thread that
    delegated it gets a system message with its outcome and last reply.
  - Since then (see "Agent tools and instructions", "Secret requests, pull
    request links and other agent toolkits" and "Issues" below): 50 tools,
    with t3code's names and inputs. Not done: t3code's preview,
    preview-controls, device, theme, project (list/create/update/delete/clone)
    and environment-link toolkits, `html_preview`, and the pull request
    toolkit's guides, create/update/review/merge and CI tools.
  - Verified: `provider_turns.rs` reads the endpoint and token from a
    scripted Claude's arguments, calls the tools over HTTP (refused without
    a token and outside the project), delegates a task that runs in a child
    thread, and sees the completion message arrive in the parent; 77
    frames pass `wire-check.ts`. Live (2026-11-19): real Claude and Codex
    call `t3_project_info`, and delegate a task with `delegate_task` and
    `t3_thread_wait`.
- **OpenCode** (ninth phase of the t3code gap plan): `adapters/opencode.rs`
  drives `opencode serve` (the v2 HTTP API, OpenCode 2.0.18 or newer;
  `DOMOVOI_OPENCODE_BINARY` names the executable). One server per Domovoi, on
  loopback with a generated password, under a watchdog shell that stops it
  when Domovoi is gone. A thread is an OpenCode session in the thread's folder:
  text and reasoning stream from `session.text.*` / `session.reasoning.*`,
  shell commands, edits and other tools become items, `permission.asked`
  becomes an approval (answered `once` / `always` / `reject`), steps report
  token use, and `session.execution.*` ends the turn. Permissions follow the
  runtime mode as t3code sets them. A message during a turn steers it;
  interrupt is `/interrupt`. The provider appears only when OpenCode is
  installed, with the models it can run (`providerID/modelID`).
  - Forms (the `question` tool) are `user_input` requests: answers go back to
    `/form/{id}/reply` (a multiselect's values, other fields' joined), a
    dismissal cancels the form (with a note to go on when a subagent asked),
    and a form Domovoi can't show is cancelled with a note to ask in chat. A
    declined permission is `reject` with a message and a steer note, so the
    agent goes on; cancel stops it.
  - Each prompt carries its own message id (`msg_t3_turn_<session>:<attempt>`,
    t3code `turnPromptId`), the provider turn's native ref. OpenCode forks
    natively: a forked thread's first turn calls `/fork` with `before` the
    source's next run's prompt (none at the latest), sets its rules and moves
    it to the thread's folder. A rollback stages and commits a revert to
    before the first undone run's prompt (`files: false`; a failed one is
    cleared).
  - A `subagent` tool call is child work (`subagents.rs`): its child session
    (`session.created` with `parentID`, named by the call's
    `metadata.sessionID`) shows its text and tools in the child thread, its
    approvals and questions open in the parent's turn and are answered to the
    child session, and its answer is the result. `subagent.stop` interrupts
    the child session. A background subagent keeps the turn open until its
    inbox report and OpenCode's wake.
  - Workspace usage limits (`/api/experimental/usage/limits`: Go's session,
    weekly and monthly meters and Console spend limits, t3code
    `readServerUsageLimits`) are read with the provider's probe and the
    periodic refresh; a `provider.rate-limit` (or non-402 `provider.quota`)
    failure fails as `usage_limit`, resetting with the used-up window.
  - Not done: steered prompts are not marked (forks and rollbacks cut at turn
    boundaries); a background report after its turn ended goes unrecorded;
    nested subagent sessions show in the top subagent's thread; "allow for
    session" is still OpenCode's `always`.
  - Verified (scripted): `provider_turns.rs` `a_scripted_opencode_turn_records_every_kind_of_item`
    (with the MCP server, rules, instructions, prompt id and the rollback's
    revert), `an_opencode_fork_continues_a_copy_of_the_session_cut_at_the_run`,
    `an_opencode_subagent_works_in_its_child_thread_and_can_be_stopped`,
    `opencode_reports_workspace_limits_and_limited_turns`,
    `opencode_questions_reply_retry_and_cancel`; 10,563 frames pass
    `wire-check.ts`. The MCP, instruction, fork, revert and subagent calls are
    checked against t3code's v2 driver and `@opencode/protocol` 2.0.28, not
    live.
  - Verified live against `opencode serve` 2.1.3 with a free local Ollama
    model: `DOMOVOI_OPENCODE_BINARY=… DOMOVOI_LIVE_OPENCODE_MODEL=ollama/qwen3.5:4b
    cargo test -p domovoi-server --test provider_turns a_live_opencode` lists the
    provider ready with the model, asks before `echo hello-domovoi`, runs it
    after the approval and replies; 4399 frames pass `wire-check.ts`; no
    `opencode serve` is left running afterwards. `DOMOVOI_TEST_STDERR=1` shows
    the test servers' logs.
- **Pi** (t3code `provider-pi`): `adapters/pi.rs` drives `pi --mode rpc`
  (JSONL over stdin and stdout, `pi_rpc.rs`; pi 0.84.4 or newer;
  `DOMOVOI_PI_BINARY` names the executable, `DOMOVOI_PI_ARGS` adds launch
  arguments, with Domovoi's own session and mode flags refused). An npm
  install of pi is a Node script, so pi runs with the `node` of its own npm
  prefix first on PATH. The provider appears when pi is installed. A
  short-lived `--no-session --offline` pi lists the models pi holds
  credentials for (`provider/id`, after a synthetic `default` that keeps pi's
  own choice), their thinking levels and service tiers, and pi's commands and
  skills (`/compact` first). A second one, without extensions, marks the
  models text generation can't load.
  - One pi per thread, kept between turns and restarted when the runtime
    mode or folder changes. The thread's native id is pi's session file; a
    turn's is the id of its user entry in pi's session tree (read with
    `get_entries` after the turn), which rollback (`fork` before the first
    reverted turn's entry) and native fork (`--fork <file>`, then `fork`
    after the source run's entry) cut at. A session file pi never wrote (a
    rollback to before the first message) starts a new session.
  - Approvals come from a bundled pi extension (`adapters/pi_extension.ts`,
    written to `userdata/caches` and passed with `--extension`). Its
    `tool_call` hook asks through `ctx.ui.confirm`, following the runtime
    mode: reads are allowed, edits too under Auto-accept edits, everything
    under full access. The extension also registers Domovoi's agent MCP tools
    (`mcp__t3__*`) and handles `$skill` chips and the service tier. Its
    `select`, `input` and `editor` dialogs become questions.
  - Each turn applies the model, thinking level and service tier, then sends
    a `prompt`. Text, thinking, bash, edit and write, other tools,
    compaction, retries and extension errors become items. A turn ends when
    pi settles (`agent_settled`, then a `get_state` that shows nothing
    streaming, compacting or queued). Context use comes from
    `get_session_stats`. Steer is a `prompt` with `streamingBehavior: steer`;
    Stop is `clear_queue` then `abort`, or ends pi during a compaction.
  - Pi sessions are scanned and imported (the active branch only, titled by
    the last `session_info`), synced, counted in usage (assistant messages'
    `usage` and `usage` entries, a fork's copied history once), and pi can be
    the text generation model (no tools or extensions, the schema in the
    prompt, provider errors shown only as their HTTP status).
  - Not done (t3code has them): extension statuses on the provider session,
    subagents as their own items (a `subagent` call is a tool item), pi's
    own work outside a turn offered as a continuation (it is stopped), and
    the orchestration system prompt (Domovoi gives none to Claude or Codex
    either).
  - Verified: `provider_turns.rs` against a scripted `pi`: every kind of item;
    the dialogs answered (accept, accept for session, then the same dialog
    remembered, decline, a select, an input); a failed, a steered and a
    stopped turn; a rollback, whose session keeps only run 1; forks at a run
    and at the latest run; and pi naming a thread and branch and writing a
    commit message. `config-providers` probes pi too old, without a version,
    without RPC, with no models, with an answer it can't read, ready, and
    with an uncredentialed default. Live against pi 1.3.0 with the local
    `ollama/qwen3:4b`: `DOMOVOI_PI_BINARY=$(command -v pi)
    DOMOVOI_LIVE_PI_MODEL=ollama/qwen3:4b cargo test -p domovoi-server --test
    provider_turns a_live_pi` lists pi ready, asks before `echo
    hello-domovoi`, runs it after the approval, replies, records the turn's
    entry, and leaves no pi running. That run and the scripted one together
    (5,142 frames) pass `wire-check.ts`. Real pi kept entry ids through
    `--fork`, and wrote no file for a session forked to before its first
    message (hence the new-session rule).

## Live turns of the real CLIs

`apps/server/tests/live/mod.rs` (a module of `provider_turns.rs`, sharing
its harness) runs each scenario against the real `claude` and `codex`, as
this user, found on PATH (or `DOMOVOI_CLAUDE_BINARY` /
`DOMOVOI_CODEX_BINARY`). A test runs only when its provider's model is
named; `just live claude`, `just live codex` and `just live both` name
cheap ones (`claude-haiku-5-5`, `gpt-6-luna`), record every frame to
`target/live-<provider>.ndjson` and decode them with `wire-check.ts`.
`DOMOVOI_KEEP_FAILED=1` keeps a failed test's home, with the provider
protocol logs under `home/userdata/logs/provider/`. Each run spends real
tokens; the prompts are a line each.

Scenarios, each for both providers: a command that asks first (and the
context used reported), plan mode then implementing the plan, a steer, a
queued message and a stop (with keep-awake held on macOS), a native fork,
a subagent, an agent MCP tool, a delegated task, text generation (title,
branch name, commit message), a turn continued after the server is stopped
with SIGTERM, an image attachment, and a scheduled task run now. `just live
both` also switches a thread from Claude to Codex.

2026-11-19, Claude Code 2.1.319 (Haiku 5.5) and codex-cli 0.170.0
(gpt-6-luna): all 23 pass; the frames (about 14,000) pass `wire-check.ts`.
What the live runs found:

- Claude Code 2.1 queues a `priority: "now"` steer as a command of its own
  (`command_lifecycle` `queued`), ends the command it was working on with a
  `result` (`stop_reason: tool_use`), then runs the steer and answers it
  with a result of its own. Domovoi ended the run at the first result, so
  the reply to the steer was lost. The Claude adapter now keeps the run
  open while a steer it sent is queued, and the scripted `claude` behaves
  as the real one does.
- Real CLIs run read-only commands (a bare `echo`) without asking even in
  approval-required mode; the approval scenario uses a command that writes.
- Codex 0.170 spawns no subagents unless its multi-agent feature is on.
- Not live yet: Codex question forms, permission and elicitation requests,
  compaction, review guides, and real APNs delivery.

## Scheduled tasks: fresh sessions and webhooks

- `freshContext` (a scheduled task's "fresh session", `t3_thread_send`):
  the run gets a provider thread of its own and no handoff; later messages
  continue it, and a provider switch hands over only the runs since. A
  scheduled task's fresh-session run is a side conversation: the user's next
  message continues the thread's own conversation (t3code
  `conversationProviderThread`). Unit tests in `orchestration/commands/tests.rs`.
- Webhook tasks (`webhooks.rs`): `POST|PUT|PATCH|GET
  /api/hooks/<task>/<token>` on the server's port, and with `--hooks-listen`
  on an address that serves nothing else; `--hooks-url` gives the public
  origin a task's `webhook.url` is shown with. In t3code's order: token (a
  wrong one is a 404, as an unknown hook), a flood guard, enabled, HMAC-SHA256
  signature over the exact body (hex or base64, prefix), filters, the replay
  key (7 days), the coalesce window (the run takes the window's last request
  when it closes; windows survive a restart), 60 runs a minute, 20 queued per
  task, and the 120,000-character prompt cap. Values are framed in
  `<webhook-data-…>` tags with mentions broken; headers and query parameters
  that name credentials are redacted in `{{request}}`, `{{headers}}` and the
  log. Each task keeps its last 50 deliveries, and 20 each of filtered and
  duplicate ones. Not ported: T3 Connect relay delivery proofs (so
  `maxDeliveryAgeMinutes` never expires a request) and managed policy.
- Verified: unit tests for placeholders, filters, signatures and redaction;
  `provider_turns.rs` `a_signed_webhook_runs_its_task_once` (wrong signature,
  wrong token, filtered, accepted with a scripted Claude turn, duplicate, a
  coalesced window run once with its last request, the log and one delivery);
  158 frames pass `wire-check.ts`.

## Agent tools and instructions

- `apps/server/src/mcp/` serves 50 tools (the newer ones in the sections below): threads (`t3_thread_list`,
  `_read` with an `activity` view, `_search`, `_send` with modes and
  `freshContext`, `_wait` for a run, `_launch`, `_configure`, `_interrupt`,
  `_update`, `_organize`, `_transfer`, `t3_queue`, `t3_pending_request`),
  delegation (`orchestrator_capabilities`, `delegate_task` with a `target`
  and `mode: "wait"`, `task_status`, `task_cancel`, `create_threads`,
  `t3_workflow_stop`), schedules (`schedule_task` that also changes a task,
  `list_scheduled_tasks`, `delete_scheduled_task`, `run_scheduled_task_now`),
  `t3_project_info`, and worktrees (`t3_worktree_status`, `_list`,
  `_handoff`). Inputs follow t3code's contracts and still take the earlier
  names (`name`, `thisThread`); `clientRequestId` makes a retried call one
  change. Claude's `--allowedTools` lists every tool annotated read-only.
- A worktree handoff moves the thread; a provider session never changes
  folders, so the next turn starts a new one there and is handed the
  conversation (also when the handoff came from the thread's first turn,
  whose run is handed over once it ends).
- `agent_instructions.rs`: two sections (each under Codex's 4,000 bytes,
  naming only tools the server has, which a unit test checks) go to Claude as
  `--append-system-prompt`, to Codex as `turn/start` `additionalContext`, and
  to Pi through its extension's system prompt, and to OpenCode as the
  session's `t3-code` instruction entry
  (`PUT /api/experimental/session/{id}/instructions/entries/t3-code`). OpenCode
  gets the tools as a remote MCP server added to the location of the thread's
  folder (`PUT /api/experimental/mcp/t3-code-<thread>`, the session's bearer
  token; t3code `t3McpServerName`), with the session's rules denying
  `t3-code-*` but its own `t3-code-<thread>_*`; a thread whose server
  OpenCode refused gets neither.
- Verified: `provider_turns.rs` `agents_organize_threads_schedule_and_hand_off_to_a_worktree`
  (every new tool over HTTP, as the scripted Claude's session) and
  `a_handoff_in_the_first_turn_takes_the_conversation_along` (fails without
  the queue's catch-up); live, real Claude and Codex call `t3_project_info`
  and hand themselves off to a worktree, where the continuation remembers a
  codeword and runs `pwd` in the worktree. The live run found the first-turn
  case.

## Skills

- Claude (`skills.rs`): the skills its last session loaded, as `system/init`
  names them (Claude's own rules choose user, project and plugin skills),
  with descriptions from the `initialize` command list, which Claude's slash
  commands now carry too. Kept in `userdata/claude-skills.json` across
  restarts. A `$name` mention of one is sent as `/name` starting the last text
  block (t3code `planClaudeSkillDispatch`; earlier user-only skills get their
  file named).
- Codex: the `SKILL.md` folders under `<CODEX_HOME>/skills` and
  `skills/.system`, from their front matter, read on each snapshot; Codex
  reads `$name` itself. t3code asks the app-server (`skills/list`) instead,
  which also sees project skills; Domovoi does not yet.
- Verified: unit tests for the mention rewrite (prices, unknown names,
  several mentions), front matter, and Claude's init as Claude Code 2.1.319
  sends it.

## Usage limits and resuming at the reset

- `usage_limits.rs`: Claude's `rate_limit_event` (its five-hour and weekly
  windows, utilization 0-1) and Codex's `account/rateLimits/updated` (the
  `primary`/`secondary` positions and a monthly credit limit; model-scoped
  limits are not the account's) update each provider's `usageLimits`, sent
  with the provider list (also in `refreshProviders` answers, which now carry
  Claude's live commands and skills too).
- A Claude turn that ends in an error while a limit rejects it (no overage
  to carry on in), and a Codex turn whose error is `usageLimitExceeded`,
  `rateLimitExceeded` or a 429 after retries, fail as `usage_limit` with the
  latest reset among the limits that stopped it.
- `thread.metadata.update` takes `limitRecovery` for the thread's latest run
  when a usage limit failed it and nothing is pending (`snooze` snoozes the
  thread until the reset); `null` clears it. `usage_recovery.rs` arms it by
  itself with `autoResumeLimitedThreads` / `snoozeLimitedThreads` (not for a
  reset more than a day past) and, once the reset has passed and the thread
  is not snoozed, sends "Continue where you left off."
  (`usageLimitContinuationOfRunId`), then clears it. It looks every 30 s.
- Limits are read before a turn reports them, and kept fresh:
  - Claude: `get_usage` when a session starts and after each turn. With no session, the probe asks a Claude Code of its own, started with no prompt, settings, MCP servers or saved session; it costs nothing.
  - Codex: `account/rateLimits/read` when the app-server starts, and from the model-discovery app-server.
  - Antigravity: `RetrieveUserQuotaSummary` from a running agent's own language server. Its HTTP port is named in `<acp>/log/acp-server-<time>-<pid>.log`, so it works with a keychain sign-in. Domovoi never reads the account's Google credentials (an older runtime's `acp_token.json` included) and never calls a Google service itself; with no agent running the last reading stays. The agent is asked at most once a minute after a turn, and once every 5 minutes otherwise.
  - Every minute while a client watches the provider list, accounts whose limits are older than 5 minutes are read again.
  - Limits are kept in `userdata/provider-usage-limits.json` across restarts, and cleared on sign-out.
  - Claude stream events below a warning threshold carry their use only in `unifiedWindows`, and are read from there.
  - The clients show a window past its reset as open, at 0%.
- Not done: reset credits (`provider.consumeResetCredit` still answers
  `noCredit`), and the system notice t3code shows in the timeline when a
  limit is hit.
- Verified: unit tests for both providers' messages as they send them;
  `provider_turns.rs` `a_usage_limited_thread_resumes_when_the_limit_resets`
  (a scripted Claude turned away; the windows on the provider; a wrong run
  refused; the continuation). Not live: no real limit was hit on purpose.

## ACP agents

- `adapters/acp.rs`: any Agent Client Protocol agent, as a provider
  instance of driver `acpRegistry` (t3code's) whose config names a local
  command (`source: "local"`, `commandPath`, `commandArgs`, and the
  instance's `environment`) or a registry agent (`agentId`). One process per
  thread: `initialize` (protocol 1; client capabilities per flavour, below),
  `session/new` in the thread's folder (`authenticate` with the instance's
  `authMethodId`, or the agent's first agent-side method, when it needs
  signing in), or `session/load` / `session/resume` of the thread's session
  when its process starts again (the replayed history is dropped).
  `session/prompt` runs a turn; `session/update` gives messages, thoughts,
  tool calls (by kind: command, file change from a diff, web search, tool),
  the plan, context use (`usage_update`) and the agent's slash commands;
  `session/cancel` stops it. `session/request_permission` asks as an
  approval (decisions pick `allow_once` / `allow_always` / `reject_once`),
  unless full access or auto-accept-edits decides it; reads are allowed.
  The model is the session's model option or `session/set_model`; plan mode
  is a mode whose id ends in `plan` or `architect`, and the previous mode
  comes back after. Domovoi's MCP tools go as an HTTP server when the agent
  takes one, else through `domovoi-server acp-mcp-bridge` (stdio); the first
  prompt of a session carries the instructions about them. A message sent
  while a turn runs waits for it (t3code restarts the turn instead), except
  for Grok and Kiro, which take it natively (below). A
  rollback restores the checkpoint and ends the agent's session: ACP can't
  truncate one, so the next turn starts a new session (t3code's
  `rollbackThread`). Sessions report the capabilities t3code's ACP adapter
  has (`provider-session.updated`).
- Instances are probed on `refreshProviders` (`initialize` and `session/new`
  in a scratch folder, for the agent's name, version and models), kept 15
  minutes; an instance removed from settings leaves the list.
- Verified: `provider_turns.rs` `an_acp_agent_runs_turns` against a scripted
  ACP agent (probe; every kind of update; an approval; model and plan mode;
  the tools over HTTP and the instructions; a failure; a stop with
  `session/cancel`; mode restored; the session loaded by a new process).
  Live: `DOMOVOI_LIVE_ACP_COMMAND="opencode acp"
  DOMOVOI_LIVE_ACP_MODEL=ollama/qwen3.5:4b cargo test -p domovoi-server --test
  provider_turns live::a_real_acp` probes OpenCode 2.1.3 over ACP (626
  models) and runs a turn that replies with a command's output.
- The registry (`acp_registry.rs`): `searchAcpRegistry` lists the agents of
  `https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json` that
  run here (a binary for this platform, else npm, else uv, each only with its
  tool on PATH; read again after 5 minutes, a copy kept for when it can't be
  reached; icons only from the registry's own host).
  `prepareAcpRegistryAgent` installs one under `<home>/acp-registry/`: an
  archive downloaded, its sha256 checked when given, unpacked (tar or zip, or
  a bare executable); an npm package installed into the agent's own folder
  (its first `bin` runs); a uv package run with `uvx`. A receipt names the
  command, which instances of `source: "registry"` start.
  `uninstallAcpRegistryManagedBinary` is refused while an instance uses the
  agent. The agent's own sessions, model providers and sign-out are answered
  with t3code's typed `*_unsupported` reasons.
- `initialize` offers what t3code offers per flavour: registry agents get URL
  sign-ins (`elicitation.url`): shown on the instance's entry as `auth.action`
  for up to 10 minutes and accepted with `server.acceptAcpRegistryUrlAuth`,
  otherwise declined. Devin alone also gets the client's terminals
  (`terminal/*`, `adapters/acp_terminals.rs`): child processes with bounded
  output (t3code's limits), refused unless the runtime mode runs commands or a
  command was approved in the turn or for the session, and ended with the
  session. Antigravity reads files of the thread's folder. No agent is offered
  `fs/write_text_file` (t3code offers it to none) or elicitation forms.
  Approvals follow t3code's `acpPermissionDisposition` and each flavour's
  adjustment of it.
- **Grok, Kiro, fx** (t3code's ACP drivers `grok`, `kiro`, `fx`) run in
  `acp_cli/`. The built-in instance is listed when its CLI is found and is off
  until settings turn it on. The probe runs `--version` under a 10 s limit,
  then each CLI's own sign-in and model checks, never opening a session. Grok:
  models and reasoning levels from `initialize`, `session/set_model` with the
  level, `_x.ai/interject` steering, question forms, plan exit as a proposed
  plan. Kiro: agent, model, effort and autopilot as config options, one-time
  approvals only, `_session/steer` steering without the model's steering note,
  its cloud-config call left out. fx: backend-qualified models, the session
  mode written on the first turn, default effort `medium`. Not done: Cursor
  (t3code drives the Node `@cursor/sdk`) and Muse (its own protocol) are not
  ACP and are not run; Kiro fork, compaction and child threads; Grok
  subagents, background tasks, skills and usage limits; fx `subProviders`;
  Antigravity steering; version advisories and installs for the three CLIs;
  a terminal's kill is SIGKILL at once (t3code: SIGTERM, then 5 s).
- Verified: `provider_turns.rs` `a_grok_agent_runs_turns`,
  `a_kiro_agent_runs_turns`, `an_fx_agent_runs_turns` and
  `a_registry_agent_uses_the_clients_terminals_and_asks_for_a_url_sign_in`
  (a scripted stand-in per CLI; a terminal refused before approval, then
  opened, waited for, read, embedded and released; a URL sign-in shown and
  accepted): 1798 frames pass `wire-check.ts`.
- Verified: unit tests for the search (what runs here, ids, icons) and a
  binary install with its checksum (and a wrong one refused); the
  `rpc-calls` and `admin-server` cases search a fixture registry
  (`conformance/acp-registry.json`, never the network), and their answers
  decode with t3code's schemas. Live: `DOMOVOI_LIVE_ACP_REGISTRY_AGENT=pi-acp
  cargo test -p domovoi-server --test provider_turns live::a_registry_agent`
  finds the 46 agents of the real registry, installs `pi-acp` through npm,
  and its instance starts and is ready.

## Antigravity

- `antigravity/` and the Antigravity flavour of `adapters/acp.rs`: Google's
  `agy` CLI (1.5 or newer) over ACP, as t3code's `antigravity` driver. A
  built-in `antigravity` instance (when `agy` is found) and instances that
  settings add. Each instance has its own profile,
  `<userdata>/providers/antigravity/<sha256(instance id)>` as `GEMINI_HOME`:
  its hooks, MCP servers and conversations are its own, and the user's
  global skills are linked in. The Google sign-in is not: agy 1.5 keeps it
  in the user's keychain (`ChainedAuth: authenticated via keyring` in its
  log), found through `HOME` (a fresh profile with a scratch `HOME` needs a
  sign-in; with the real one it has the user's), so it is the machine's
  `agy` sign-in, shared with the terminal and the IDE. Only the built-in
  instance signs in with Google; added instances use the API key or Vertex
  methods. The agent starts as `agy acp --gemini-dir <profile>
  --app-data-dir antigravity-acp`, without the server's Google credentials,
  with a temp folder of its own per launch. The native CLI reads no
  `BROWSER`: it opens its sign-in link with `open` / `xdg-open` from its
  PATH (seen with a recording `open`), so each launch has its own first on
  PATH, which hands Google links to the server (through a file: the CLI
  discards that child's stderr) and passes anything else to the real one.
  `authenticate` with the instance's method at every launch; a sign-in link
  in a thread's launch means "sign in first". Sessions resume (never load).
  The model is the account's (from the session's model select; an older id
  of a listed family, like the default `gemini-3.8-flash-high`, is taken
  too); the thinking level is a model option, applied with
  `session/set_config_option`. The session mode stays `default` and
  approvals are answered from the thread's runtime mode; native questions
  (`interaction_…`) are question forms; the agent reads the thread's folder
  and the attachments through `fs/read_text_file`. Tool payloads are
  bounded, edits placed at their only match in the file (else marked as
  snippets), reads the client served shown done. A turn's real outcome
  comes from the agent's session log (a quota, an API error it reports as a
  reply), a usage limit failing the turn with its reset. A stop waits 15
  seconds for the agent before it is killed.
- Sign-in (`provider.auth.*`): a throwaway agent in a scratch folder; the
  Google link it opens is checked (`/o/oauth2/auth` with PKCE and a
  `localhost` `/auth/callback` redirect, as agy 1.5.14 sends it, or t3code's
  `/o/oauth2/v2/auth` form) and shown to the client that started it; a
  pasted redirect is checked against the flow and delivered to the agent's
  loopback listener; then `session/new` checks access, and each model is
  selected once for its thinking levels. The API key and Vertex methods
  connect the same way; the key goes to `<userdata>/secrets` and settings
  keep `••••••`. Domovoi signs nothing out: `provider.auth.logout` and
  `/logout` in a thread say to use `/logout` in `agy` itself (signing the
  shared account out would sign out the terminal and IDE too), and
  `canLogout` is false. The account's plan (`authenticate`'s
  `_meta.antigravity.tier`, e.g. "Google AI Pro") is its label. The account
  (signed in or not, models, commands) is kept in the profile across
  restarts; the probe never starts the agent.
- Limits: only through a running agent's own language server
  (`RetrieveUserQuotaSummary`: Gemini and shared Claude & GPT, 5-hour and
  weekly), read in the background. A sign-in saved in the profile
  (`acp_token.json`) is never read, and Domovoi sends nothing to Google
  itself: using Antigravity's sign-in from other software is against its
  terms. The agent isn't told to save its sign-in in a file
  (`AGY_ACP_FORCE_FILE_STORAGE` is only removed from what it inherits). Usage history: the conversation
  databases of the user's Antigravity apps and of the profiles. Skills:
  `SKILL.md` folders of `~/.gemini/config/skills`,
  `~/.gemini/antigravity-cli/skills` and a project's `.gemini/skills`,
  `.agents/skills`, `.agent/skills`. Text generation: a fresh agent in an
  empty folder that may use no tool. Subagent batches (`invoke_subagent`)
  are subagents of the turn; `orchestration.getNativeChildWork` reads their
  children from the parent session's log.
- Managed install (`provider.install.*`): Google's release channel names
  the release for this platform; it is downloaded, its SHA-512 checked,
  unpacked (one regular file), started once to prove it, then made active
  (`active.json`). Not removed while an agent runs from it.
- The native apps (Settings > Providers on Mac, iOS and Android) sign in,
  install, update and remove the runtime, edit an instance's sign-in method,
  key, project, location and binary path, and add or remove API-key and
  Vertex instances. The core follows `provider.auth.subscribe` and
  `provider.install.subscribe` while the page is open, sends
  `provider.auth.*` / `provider.install.*`, and edits instances with a
  `server.updateSettings` `providerInstanceMutation` that keeps the
  instance's other settings and sends a key only when one is typed (the
  server shows a saved key as `••••••`). A sign-in link opens in the
  device's browser; from another device the redirect URL is pasted back.
  `client_replay.rs` `a_provider_signs_in_installs_and_is_edited_from_settings`.
- Verified: unit tests in each module, and `provider_turns.rs`
  `an_antigravity_agent_runs_turns`, `an_antigravity_instance_signs_in_and_out`,
  `antigravity_turns_show_edits_reads_and_limits`,
  `antigravity_reports_limits_skills_titles_and_batches` and
  `antigravity_is_installed_from_googles_release_channel` against a
  scripted `agy` (and stand-ins for Google's APIs and release channel);
  their frames decode with t3code's schemas. The usage reader read this
  machine's real Antigravity history (71 calls of Gemini 3.1 Pro and Flash
  Lite).

## Clients: scheduled tasks, limits

- The core (`ScheduledTaskEdit`, `SaveScheduled`): one editor creates and
  changes a task with every setting it has: at a time on chosen weekdays,
  once, every so often, or on webhook requests; a new thread each time, or
  one thread continued or started fresh; the project folder or a new
  worktree from a base branch; a model (or the composer's), permissions,
  plan mode, enabled. `RotateWebhookToken` and `LoadWebhookDeliveries` serve
  a webhook task's URL and its log. The list shows the schedule with its
  weekdays, the last error and a webhook task's URL. `PlanForLimitReset`
  (resume, or snooze) and `CancelLimitPlan` act on a thread a usage limit
  stopped, whose page shows when the limit resets; providers show their
  limits in a line.
- The Mac, iPhone and Android apps use them: the editor (a sheet on the Mac
  and iPhone, a bottom sheet on Android) for new and existing tasks, webhook
  URL copy, new URL and deliveries, the usage-limit banner at the end of a
  thread, and a provider's limits in Providers. Android's interval is now the
  same list of presets as the Apple apps (it was 1 to 168 hours).
- Verified: `client_replay.rs` records the four new requests (a task with
  every setting, one changed to a webhook, a new URL, deliveries) and checks
  the rest are unchanged; the Mac and iPhone (simulator) apps and the
  Android APK build, Android's unit tests pass; `just mac-snapshots` captures
  the Scheduled tab and the editor (`13-scheduled-editor`). Not driven on a
  device or emulator by hand.


## Re-pin to t3code v0.1.11

- The reference checkout moved from `f313d0a` (2026-11-13) to the `v0.1.11` release
  (`59651c1761`, 2026-11-20). `conformance/t3code-pin` holds the pin; `just reference`
  checks it out and installs its packages, and CI reads the same file.
- What changed in the contract (`conformance/rpc-inventory.ts` before and after, and
  `git diff f313d0a v0.1.11 -- packages/contracts`): 15 new methods and none removed —
  `issues.list|detail|comments|templates|candidates|create|comment|update|setState|react|linkedThreads`,
  `issueTrackers.list|connect|bindProject` and `server.getSettings`; two new commands,
  `thread.issue.link` and `thread.issue.unlink`, with `issues` on the thread and its
  shell and the `thread.issue-synced` event; five new settings
  (`requireConnectDeviceApproval`, `enablePreviewTunnel`, `defaultAppearance`,
  `defaultAppearanceSetAt`, `usageHistoryRetentionDays`); `server.updateSettings` now takes
  `ClientServerSettingsPatch`, which leaves out the theme defaults; and optional fields
  elsewhere (`ServerConfig.hostname`/`serverTime`, provider `subProviders`, run and turn
  capabilities, `thread.visit-recorded`), which older servers may leave out.
- Done: the new settings in `ServerSettings` and the bundled defaults
  (`gen-settings-defaults.ts`), the permission table (`rpc-scopes.ts`, 235 methods),
  `server.getSettings`, the theme keys dropped from a client's patch, the issue links on
  threads (link once per tracker-level key, unlink by key, as t3code's orchestrator), and
  the issue methods registered (see "Issues").
- Verified: with Node 26 (`DOMOVOI_NODE`; Node 22 skips the schema checks without failing),
  `cargo test -p domovoi-server -p domovoi-model -p domovoi-contracts -p effect-rpc` passes,
  `every_listed_rpc_answers_with_its_expected_kind` decodes 644 messages of 226 methods with
  v0.1.11's schemas, all 46 RPC groups (`just rpc-groups`) pass with their schema checks, and
  both golden sessions still decode (`wire-check.ts`). Not done: a new recording of the
  TypeScript server at v0.1.11 (the golden sessions are still the f313d0a ones), and the
  manual acceptance run with the v0.1.11 web client.

## Operations: `/health`, `/ready`, compression, protocol check, `doctor`, provider CLI updates

- `/health` answers 200 `{"status":"ok"}` whenever the process serves HTTP;
  `/ready` answers 200 `{"status":"ready"}` once the database answers and
  startup recovery has settled the runs a stop left open, and 503
  (`recovering` or `database_unavailable`) before that. Both are
  unauthenticated and Domovoi's own (t3code has neither), for systemd and
  monitoring; over iroh an unpaired peer still gets 401.
- HTTP responses are gzip or brotli compressed when the client sends
  `Accept-Encoding` (through iroh too), except the RPC socket, webhook URLs,
  images other than SVG, event streams and already-compressed media.
- As t3code's `ws.ts`, `/ws` refuses a URL without `orchestrationProtocol=2`
  with HTTP 426 `orchestration_protocol_incompatible`, before any credential is
  checked. The core and t3code 0.1.11 send it.
- `domovoi-server doctor [--json]` checks a home folder without changing it:
  the folder, database integrity, provider CLIs (`--version`, 10 s each), git,
  git-lfs, gh and its sign-in, the iroh key and remote setting, autostart and
  the systemd service, and the port; a failed check makes the exit code
  nonzero. (t3code's `t3 doctor` is a database tool; this one differs.)
- `server.updateProvider` updates the built-in Claude, Codex, OpenCode and pi
  CLIs as t3code's maintenance runner does (`provider_update.rs`): npm global,
  Homebrew or the CLI's own updater, chosen by where the CLI is installed,
  with sampled progress in `updateState` on every client's provider list, one
  update per installation, and the provider probed again afterwards.
- Not done: managed Codex installation (`provider.install.*` for managed setup
  mode still refuses); latest-version lookups and version advisories;
  `targetVersion` (refused: no model manifest); draining running sessions
  before an update; bun, pnpm, yarn, Volta and mise updaters; WinGet, Scoop
  and Chocolatey installs (left to their managers; npm's `.cmd` shims and the
  native `.exe`s are updated); the `x-t3-orchestration-protocol` header on HTTP snapshot
  routes; relay dialling and database compaction in `doctor`.
- Verified: `apps/server/tests/ops.rs` (health and ready; gzip and brotli
  decoded and compared; the 426 cases; `doctor --json` against a temp home with
  stand-in tools and against a running server; `updateProvider` with a fake
  npm, its streamed progress, "already running", failure and refusals);
  `remote_access` (gzip through the iroh bridge); RPC group
  `config-provider-update` with the schema check; unit tests in `doctor.rs` and
  `provider_update.rs`.

## Secret requests, pull request links and other agent toolkits

- `request_secret` shows a secret card (`secret_request.record`, server-only:
  clients' `orchestration.dispatchCommand` refuses it) and waits for
  `secrets.answerRequest`, which stores the value owner-only under
  `<userdata>/secret-requests/` behind a one-use HMAC `secret-ref:`, then marks
  the card saved or declined. Events, projections and tool results carry only
  the status, the ref, or (`delivery: "file"`) the path of an owner-only file;
  values and files go after 24 hours. A webhook `schedule_task` spends a
  `signature.secretRef`. The card closes as cancelled on timeout, when its run
  ends or restarts, or when the call goes away. Not done: secret providers
  (`reference` answers fail with `reference_unreadable`); Domovoi's native
  clients show the card without an answer field (the t3code web client
  answers it).
- `thread.pull-request.link` / `unlink` / `watch` follow t3code's
  `Orchestrator.ts`: one link per lowercased host/repository/number, stack
  members unlinked to `stack-dismissed` tombstones, watches that carry over
  the last watch's news, `until: "merged"`, `checksPending`, and refusals for
  settled, archived and subagent threads. The pull request tools
  (`link_pull_request(s)`, `unlink_pull_request(s)`,
  `list_thread_pull_requests`, `watch_pull_request`, `unwatch_pull_request`,
  `pull_request_read`) read summary and checks through `rpc/source_control`
  (`gh`). Not done: no watcher polls hosts or wakes threads; no derived stacks;
  no guides, create, update, review, merge or CI tools.
- Attachments (`t3_attachment_prepare_upload`, `t3_attachment_discard`,
  `t3_thread_send_attachments`) go through signed upload URLs; only the
  preparing thread can send or discard a pending upload (kept in memory, so
  not across a restart). `html_render` publishes an `html` artifact (no
  `html_preview`: no headless browser). `send_to_user` and `tool_search` as
  t3code's.
- Verified: `provider_turns.rs`
  `agents_ask_for_secrets_link_pull_requests_and_send_attachments` (every new
  tool over HTTP as a scripted Claude session; the saved value is found in no
  event and no file outside the secret store); orchestrator and
  `secrets::requests` unit tests.

## Issues

- `issues.*` and `issueTrackers.*` (t3code v0.1.11 `IssueService`,
  `IssueTrackerConnections`), in `rpc/issues.rs` with `issues/github.rs`,
  `issues/linear.rs` and `issues/trackers.rs`. A project's issues are its git
  host's (GitHub, through `gh`: GraphQL reads, REST writes), beside the Linear
  team it is bound to in Settings (Linear's GraphQL API; the API key is
  checked before it is stored in `<userdata>/server-secrets/`; bindings in the
  `issue_tracker_bindings` table; disconnecting unbinds every project). A
  listing reads each scope once and carries per-scope cursors, viewers,
  tracker summaries and per-project errors. Another repository on the same
  host is read-only, and every write is checked. `issues.linkedThreads` comes
  from the store (most recently active first). Agents get t3code's issues
  toolkit (`list_issues`, `read_issue`, `create_issue`, `comment_on_issue`,
  `set_issue_state`, `link_issue`, `unlink_issue`, `list_thread_issues`);
  `link_issue` links with source `agent`.
- Not done: Azure DevOps and Forgejo issues (GitLab: see "GitLab"; listed as hosts that
  cannot be browsed yet); GitHub YAML issue forms; paging labels and assignees
  past 100 and closing pull requests past 20; `retryAt` for GitHub rate
  limits; read caches (`refresh` changes nothing).
- Verified: RPC groups `issues-reads`, `issues-writes`, `issues-failures` (no
  `gh`, signed out, rate-limited, unreadable answers), `issues-trackers`
  (Linear against a local stand-in on port 47613: connect, reject, rate limit,
  bind, list beside GitHub, every read and write, revoked key, disconnect) and
  `issues-mcp` (all eight tools as an agent), each decoded with t3code's
  schemas; unit tests in `rpc::issues`, `rpc::issues::{github,linear}`,
  `mcp::issues` and `secrets`.

## Managed policy

- An administrator's policy file (t3code `ManagedPolicyDocument`, `policy/*.ts`;
  `policy.rs`), read once at startup from `/Library/Application
  Support/Domovoi/policy.json`, `/etc/domovoi/policy.json` or `C:\Program
  Files\Domovoi\policy.json` (debug builds also `DOMOVOI_DEV_MANAGED_POLICY_PATH`,
  as t3code's `T3CODE_DEV_MANAGED_POLICY_PATH`); no file, no policy. A file this
  build cannot fully enforce (unknown keys at any level, schema mismatches,
  settings entries t3code would refuse) stops the server with exit code 77 and
  t3code's `ManagedPolicyFileError` message. Clients get `ManagedPolicySummary`
  in `ServerConfig.managedPolicy`.
- Settings: `settings` defaults replace the built-in ones and locks are laid over
  the settings (in project overrides too); `server.updateSettings` refuses moving
  a lock (`ServerSettingsError`, `write-managed-setting`); the user's own values
  stay in `settings.json`. Remote clients: `limits.clients` without `remote`
  refuses `--host` beyond loopback and `--remote`, never starts iroh, makes no
  remote pairing links and signs out iroh-bound sessions; without
  `local-browser` the server does not start. `connect: false` (or no `remote`)
  refuses `cloud.installRelayClient`. `webhookTasks: false` sets
  `blockedByPolicy`, refuses enabling such a task, and its URL answers 403
  `blocked_by_policy`. `providers.allowedDrivers` shows other drivers as
  unavailable, refuses their turns and refuses turning one on
  (`use-disallowed-provider`).
- Not done: `limits.maxRuntimeMode` and `limits.providers.instances` are refused
  at startup (exit 77) rather than enforced; the macOS configuration profile and
  Windows registry policy are not read; no `policy show`; `telemetry`,
  `browserCookieImport` and `updates` are reported only (Domovoi has nothing they
  switch off); `previewTunnel: false` switches the preview tunnel off; a value
  the user chose equal to the policy default reverts to the built-in default when
  the policy goes (t3code's `keepStored`).
- Verified: `apps/server/tests/managed_policy.rs` (summary, locks with the file
  left as the user wrote it, a disallowed provider, a blocked webhook task and its
  403, the relay refusal, exit code 77 for an unknown limit and for a ceiling, the
  `--host 0.0.0.0` and `--remote` refusals; every frame decoded with t3code's
  schemas) and unit tests in `policy.rs`.

## Peer links

- `peerLinks.link`, `list` and `unlink` over iroh instead of HTTPS or T3 Connect
  (`peer_links.rs`; Settings → Labs → Linked environments). A link is made from
  another Domovoi server's `domovoi://connect` link or ticket and a pairing code
  from it; the QUIC handshake proves the peer's key, so the code and the token
  reach only that key. Links dial with their own key (`<home>/remote/peer.key`)
  and are kept in `<userdata>/peer-links.json` (0600) with tokens in
  `<userdata>/server-secrets/`. The session is narrowed to the access chosen
  (`read-only`: `orchestration:read`; `full-access` adds operate,
  `settings:write` and `source-control:write`). The peer lists the link as a
  client (`Domovoi · <label>`) and can revoke it, after which it lists as
  `expired` with `lastError`; unlinking signs the session out there too. Agents
  get `t3_environment_links`, `t3_environment_unlink`, and `t3_thread_list` with
  `environmentId`, which reads the peer's shell through the link.
- Not done: link request cards (`t3_environment_link`; `answerRequest` only
  checks the card), forwarding the other thread tools, `delegated_task.remote.*`
  delegation, the middle access levels, http(s) addresses.
- Verified: `apps/server/tests/peer_links.rs` with two servers on `--remote
  --relay disabled` (disabled, web-address, ceiling, self, wrong-code,
  wrong-environment and spent-code refusals; a read-only link, its listing and
  scopes on the peer; revocation on the peer; a full-access relink; unlink signing
  out on both sides; 40 frames decoded with t3code's schemas) and unit tests.

## GitLab

- Pull requests and issues of a project whose repository is on GitLab go through
  the GitLab CLI (`glab api`, after t3code's `source-control-gitlab`:
  `GitLabCli`, `GitLabPullRequestCli`, `GitLabPullRequestProvider`,
  `GitLabIssueTracker`). `crate::glab` names the host in `GITLAB_HOST` and puts
  JSON bodies on stdin. A repository identity's kind picks the driver
  (`rpc/source_control/gitlab.rs` beside `github.rs`, `rpc/issues/gitlab.rs`
  beside `issues/github.rs`); a host whose name has a `gitlab` label is GitLab
  Self-Hosted, as t3code's `recognizeRemote` has it.
- Merge requests: REST listings with GitLab-side filters, GraphQL approval and
  pipeline states, line counts, summary, detail (merge settings, label
  permission, divergence, rebase, auto-merge), checks as pipeline stage jobs,
  activity (notes, line and overview threads, outdated threads, commits, awards,
  edits, bots), diff file contents, change breakdown, filter options (`#` and `!`
  references), reviewers and labels, edits, comments, replies, resolution,
  reactions, reviews (request-changes, positioned discussions, note, approve),
  and the actions merge, auto-merge on/off, ready, draft, close, reopen and
  rebase. `glab` missing → `cli-missing`; signed out or an expired login →
  `cli-unauthenticated`. Issues: listings with page cursors, detail with notes,
  system and label events, closing and mentioning merge requests, awards,
  comment pages, templates, candidates, create, comment, update, Open/Closed,
  reactions and GitLab issue URLs. The agent's `pull_request_read`,
  `link_pull_request`, `list_issues` and `read_issue` work for GitLab projects.
- Not done: the pull request diff route (Domovoi serves none for any host),
  `revert` and `delete-branch`, the merge head guard and credit removal, GraphQL
  listing and list cursors, bridge-job checks, ranged review comment line codes,
  the fork maintainer-edits permission, viewed files, paged review threads,
  self-managed hosts known only through `glab auth status`, `retryAt` for rate
  limits.
- Verified: RPC groups `prs-gitlab-reads`, `prs-gitlab-writes`,
  `prs-gitlab-failures`, `issues-gitlab` and `issues-gitlab-mcp` against a
  stateful stand-in `glab` (`gitlab-fixtures/`), each decoded with t3code's
  schemas; all other groups still pass (three listing cases now expect GitLab's
  `cli-missing` where a GitLab project without `glab` is listed, as t3code ranks
  host errors); unit tests in `glab`, `rpc::source_control::{gitlab,
  pull_requests, pull_request_failure}` and `rpc::issues::gitlab`.

## Preview

- `/api/preview/tunnel?port=<n>` (t3code `PreviewTunnel.ts`, `preview/tunnel.rs`):
  raw TCP from an authenticated client to this machine's loopback ports, one
  connection per WebSocket, over TCP or iroh. As t3code's: `terminal:operate`
  (ticket, bearer or cookie); 400 for a bad port or a non-upgrade; 403 with
  `x-t3-tunnel-disabled: 1` while `enablePreviewTunnel` is off or the managed
  policy sets `limits.previewTunnel: false`; 403 with
  `x-t3-tunnel-port-refused: 1` for the server's own port; 502 with it when
  nothing accepts on 127.0.0.1 or ::1 within 5 s. Only a port is read, never a
  host. A tunnel closes with its session and when the tunnel is switched off.
- The server's headless browser (`preview/browser.rs` over a minimal DevTools
  client, cargo feature `preview-browser`, on by default: +0.9 MB release
  binary, +1.65%): `DOMOVOI_PREVIEW_BROWSER_BINARY`, a `chrome-headless-shell`
  t3code installed in the home, or a local Chrome, Chromium or Edge; without one
  the browser answers t3code's unavailable message. (t3code downloads a pinned
  `chrome-headless-shell` and drives it with Playwright; Domovoi uses a browser
  already on the machine.) Server tabs (`runtime:"server"`) start at once, are
  marked `headless` and report their status to the manager; resizes, zoom,
  colour scheme, cookie/cache clearing and hard reloads apply; a page that
  closes itself closes its tab; one process per persistent profile
  (`<userdata>/server-browser/profiles/<id>`), incognito in a fresh context;
  `preview.reportProfiles` is saved and `preview.clearProfile` deletes the
  profile. Control as t3code's `SessionControl`; 8 tabs per agent, 32 per
  server, idle agent tabs close after 30 minutes, idle browsers after 5.
- `/api/preview-stream/ws` (t3code `ServerBrowserStream.ts`): screencast JPEGs
  paced by the client's `ack`, `viewport` / `control` / `probe` messages and
  t3code's viewer input; 4404 when the tab is missing or closes, 503 when the
  browser cannot start; `interactive=false` or no `preview:operate` only
  watches.
- Agent tools: `preview_status`, `preview_open`, `preview_dialog`,
  `preview_navigate`, `preview_resize`, `preview_set_appearance`,
  `preview_snapshot` (bounded as t3code's; image content or a PNG under
  `<userdata>/browser-artifacts/`), `preview_act`, `preview_evaluate`,
  `preview_wait_for`, `t3_preview_list`, `t3_preview_close`, gated by "Agent
  browser access"; `html_preview` (PNG, content height, console).
- Not done: browser download/install; Linux sandbox and library diagnosis
  (4503); recording; popups as tabs; file chooser interception (upload by
  locator only); downloads; clipboard, pointer, popup and download stream
  messages; desktop-rendered tabs and desktop automation hosts; reopening a
  crashed page; Playwright's aria snapshot (refs come from a flat element list);
  `html_preview` image inlining and theme fragment; subagents in one thread
  share its tabs.
- Verified: `apps/server/tests/preview.rs` (tunnel against a TCP echo server:
  every refusal, ticket, switch-off and sign-out; through an iroh bridge; the
  browser through RPC and the stream against a local site with the installed Google Chrome on
  this Mac, its RPC answers and `subscribePreviewEvents` chunks decoded with
  t3code's schemas; without a browser); `provider_turns`
  `agents_drive_the_server_browser` (every tool as a scripted Claude session's
  agent); browser tests skip without Chrome or Chromium; unit tests in
  `preview::{tunnel,chrome,page,browser}`, `mcp::preview` and `rpc::preview`;
  `rpc-calls/preview-device.json` and RPC group `admin-preview`.

## Secret request cards, linked issues and pull requests in the native clients

- The core shows a thread's pending `secret_request` items as cards
  (`ThreadPageView.secret_requests`: label, reason, link, placeholder, and a line
  on how the value is delivered) and answers them with `secrets.answerRequest`
  (`AnswerSecret` saves, `DeclineSecret` declines). The value is sent once and
  kept nowhere: not in the model, the outbox or anything saved; not sent again
  after a dropped connection (the card can be answered again); left out of
  `Request`'s debug output and of the Iced frame trace. The row says how the
  request ended (saved, declined, no longer needed). macOS and iOS use a
  `SecureField`, Android a password field (not `rememberSaveable`), Iced a secure
  `text_input`; each clears it once sent.
- `ThreadPageView.links`: the pull requests (number, title, state; stack members
  the user dismissed left out) and issues (`displayKey`, title) linked to the
  thread, shown as chips that open in the browser.
- Not done: `reference` answers (secret providers); values are not zeroized in
  memory.
- Verified: `client_replay` `secret_requests_are_answered_once_and_kept_nowhere`
  (payload as t3code's schema, no outbox or store, no resend, refusal reasons)
  and `linked_pull_requests_and_issues_show_on_the_thread`; Iced UI test
  `secret_requests_take_a_hidden_value_and_links_open_in_the_browser`;
  `tools/parity.py`; macOS, iOS (simulator) and Android builds, Android unit
  tests.

## Interactive artifact previews in the desktop app (`webview` feature)

- Built with `--features webview`, the Iced app opens interactive HTML, bundle
  and React artifacts in a preview window of its own (system webview through
  `wry`/`tao`, in a helper process that ends with the app), falling back to the
  browser if that window can't start. Without the feature they open in the
  browser as before. The window is not embedded in the panel: iced 0.14 owns the
  only event loop, a webview can't leave the main thread, and a native child view
  would cover iced's palette and menus (`apps/domovoi-iced/src/webview.rs`).
- Not done: page errors back into the app; following a theme switch; Windows and
  Linux builds (Linux needs WebKitGTK 4.1 and GTK 3 dev packages); a run by hand.
- Verified: built and its unit test run on macOS; the lint gate with and without
  the feature.

## Android push over UnifiedPush (Web Push)

- An Android device with a UnifiedPush distributor (ntfy, NextPush, …) registers
  its endpoint and Web Push keys with each server through
  `POST /api/domovoi/push/webpush` (`DELETE` removes it, `GET` gives the server's
  VAPID key; `push_web.rs`). The server sends the same "Needs your approval",
  "Finished" and "Failed" alerts as APNs to devices whose app has no socket open,
  as RFC 8291 `aes128gcm` messages signed with VAPID (RFC 8292; the key is made on
  first use, `<home>/webpush-vapid.p8`, owner-only), and forgets endpoints that
  answer 404 or 410. Approvals can be allowed or declined from the notification.
  No Google services; FCM is not used.
- Not done: answering questions from the notification; badges; retries; passing
  a VAPID key to the distributor; refusing plain-`http` endpoints; a run on a
  device with a real distributor.
- Verified: `apps/server/tests/webpush.rs` (mock push service: the failed turn's
  message decrypted with the phone's key, VAPID audience and signature, an open
  app skipped, a 410 endpoint forgotten, registration checks); `push_web` unit
  tests (RFC 8291 Appendix A ciphertext, VAPID key reuse and file mode);
  `client_replay` `push_endpoints_register_with_the_server_in_use`; Android
  `UnifiedPushServiceTest`.

## Usage pricing, the trace file and diagnostics, attachment document text

- Usage summaries price every record from LiteLLM's rate table
  (`usage_pricing.rs`, after t3code's `usagePricing.ts`): kept in
  `<userdata>/usage-model-rates.json` with its ETag and Last-Modified, fetched
  again after a day (or a minute after `server.refreshUsageRates` or a model the
  table doesn't list); `usagePriceOverrides` win, `usageModelAliases` remap the
  model (the filter matches the model as logged), and `pricing.status` is
  `fresh`, `cached` or `unavailable`. Buckets carry cost, cache savings, category
  costs, fast and premium amounts and unpriced counts as t3code's. Usage of
  deleted transcripts keeps counting for `usageHistoryRetentionDays`.
  `DOMOVOI_USAGE_RATES_URL` replaces the source (tests use
  `conformance/usage-rates.json`, never the network).
- Every RPC call is a `ws.rpc.<method>` span (`effect-rpc`), written by
  `trace_writer.rs` to `<userdata>/logs/server.trace.ndjson` in t3code's
  `EffectTraceRecord` format (bounded queue, batched off the request path,
  10 MiB per file, 10 files kept); with `observability.otlpTracesUrl` set the
  spans are also exported as OTLP/JSON. `server.getTraceDiagnostics` summarizes
  the files (`trace_diagnostics.rs`, t3code's `TraceDiagnostics.ts`).
- Uploaded PDF, DOCX, XLSX and PPTX attachments get a `<file>.txt` sidecar from a
  hidden `__document-text` child process (`document_text.rs`: 15 s timeout, input,
  output, archive and expansion caps after t3code's; a zip bomb is truncated, a
  malformed file rejected), started at upload and awaited before a turn or steer
  that carries the file; the prompt names it after the file ("text extracted
  to: …").
- Not done: Codex's fast tier and Claude advisor iterations as separate records;
  the retention cache does not survive a restart; ODT/ODS/ODP, RTF, EPUB and
  notebook text (PDF text is what `lopdf` gives; scans give none); span links,
  URL credential redaction and `OTEL_*` resource attributes; spans other than RPC
  calls; a release-profile overhead figure (dev profile: about 4–5 µs per span
  with the trace file, measured in alternating pairs on a loaded machine).
- Verified: unit tests for price math, tiers, speeds, aliases, overrides, unknown
  models, ETag/304 against a local server, retention, trace records, rotation,
  the OTLP body and export, the diagnostics aggregator, every document format,
  zip-bomb and malformed input, a hung or oversized worker, and the prompt naming
  the sidecar; `tests/document_text.rs` runs the real worker; RPC groups
  `workspace-usage`, `admin-server` and `config-settings` pass t3code's schemas.
