# Verifying domovoi-server against the real t3code client

The acceptance test is t3code's own web client, unmodified, talking to `domovoi-server`,
with every WebSocket message checked against t3code's Effect schemas.

1. Check out t3code (`pingdotgg/t3code@f313d0a`) under `.reference/t3code` and run
   `pnpm install` there (Node 26).
2. Start the Rust server with a throwaway home:

   ```sh
   cargo run -p domovoi-server -- --home-dir .reference/domovoi-home --port 13774 --web-url http://localhost:5733
   ```

3. Put the recorder in front of it:

   ```sh
   cargo run -p ws-tap -- --listen 127.0.0.1:13999 --upstream http://127.0.0.1:13774 --out rec.ndjson
   ```

4. Start t3code's web dev server with its proxy pointed at the recorder
   (`pnpm dev:web --home-dir <scratch> --port 13999` in `.reference/t3code`), open the
   `pairingUrl` domovoi-server printed, and drive the app. For another code while the server
   runs: `cargo run -p domovoi-server -- --home-dir .reference/domovoi-home --port 13774 pair`
   (add `--permission orchestration:read` for a read-only client).
5. Validate the session:

   ```sh
   node conformance/wire-check.ts rec.ndjson
   ```

   It decodes every request payload, stream chunk and exit with t3code's `WsRpcGroup`
   schemas and fails on the first mismatch.

`conformance/run.sh` separately checks the `effect-rpc` crate against Effect's own
`RpcClient`.

## Automated checks

- `cargo test --workspace`: unit tests plus `apps/server/tests/golden_replay.rs`, which
  starts the `domovoi-server` binary in a throwaway home, pairs over HTTP, opens the
  authenticated WebSocket, replays all 70 client requests recorded from the TypeScript
  server (`fixtures/golden/ts-session2-boot`), and requires each to get the same kind of
  answer (chunk / success / failure / none). With `.reference/t3code` and Node 26+
  present it also decodes every Rust response with t3code's schemas. A deliberately
  broken handler makes it fail (`server.probe: t3code=exit-success domovoi=exit-failure`).
- `apps/server/tests/rpc_calls.rs`: runs every case in `conformance/rpc-calls/*.json`
  (one file per area, 312 calls) against the real binary, requires the expected
  answer kind (first chunk / success / typed failure with a named `_tag`), and decodes
  every request and answer with t3code's schemas: 219 of the 220 methods
  (`subscribePreviewEvents` sends nothing until a preview tab changes; unit tests
  cover its events). The server runs with a system-only `PATH` and an empty agent
  home, so no case reaches a provider, `gh`, an editor or the real `~/.claude`.
  `DOMOVOI_CALLS=<area>` runs one file. `permissions::every_t3code_method_is_registered`
  checks the router answers all 220 `WsRpcGroup` methods.
- `just rpc-groups` (ignored test `every_rpc_group_answers_as_recorded`): the 46 files
  of `conformance/rpc-groups`, each on a server of its own with stand-ins for `gh`,
  `ssh` and the provider CLIs: 202 methods called for each answer and failure they can
  give (2,199 answers), every in-schema one decoded with t3code's schemas. Six to ten
  minutes; `DOMOVOI_TURNS_DUMP_DIR` writes the answers, to compare two builds.
- `conformance/run.sh`: `effect-rpc` against Effect's own `RpcClient`.
- Remote access over iroh:
  - `crates/domovoi-remote/tests/loopback.rs` (two endpoints, no network): HTTP,
    concurrent connections, a WebSocket echo, the peer id reaching handlers, a bad
    ticket failing at start, and redialing after the server restarts. `-- --ignored`
    adds two live checks over n0: dialing by endpoint id alone, and relay fallback
    (a client with no direct paths gets HTTP and WebSocket through the relay).
  - `rpc_calls.rs` runs all 312 cases a second time, and `golden_replay.rs` the
    recorded session, through an iroh bridge to a server started with
    `--remote --relay disabled`, schema checks included. `rpc_calls.rs` waits
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

## Performance baseline

Measured 2026-11-16 at `dc22ef9` on an Apple M5 Max, optimized builds. These
are the reference numbers for the performance work; rerun the same commands
after a change and compare.

```sh
cargo bench -p effect-rpc --bench frames        # wire codec over ts-session1
cargo bench -p domovoi-model --bench thread          # thread read model
cargo bench -p domovoi_core --bench view             # view() + bincode, per render
cargo test --release -p domovoi-server perf_ -- --ignored --nocapture --test-threads=1
cargo test --release -p domovoi-server --test provider_turns perf_ -- --ignored --nocapture --test-threads=1
```

**Wire codec** (`fixtures/golden/ts-session1`: 886 client frames, 809 server
messages, 3.8 MB): decoding every client frame 0.94 ms, encoding every server
message 2.85 ms, splitting them with `split_socket_frame` 4.48 ms. Splitting
costs more than encoding.

**Thread read model** (`domovoi-model`, medians):

| Items | `shell()` | one streamed token (apply + shell) |
|---|---|---|
| 100 | 0.16 ms | 0.25 ms |
| 1000 | 1.62 ms | 2.42 ms |
| 5000 | 9.21 ms | 13.9 ms |

**Server store** (file-backed SQLite):

| Entities | append one event | `thread_shell` (shell stream, per event per subscriber) |
|---|---|---|
| 100 | 0.10 ms | 0.31 ms |
| 1000 | 0.09 ms | 3.08 ms |
| 5000 | 0.10 ms | 28.2 ms |

- `thread_events_after` for 50 events of one thread: 0.14 ms alone, 28.8 ms after
  10,000 events on other threads, 144 ms after 50,000. It scans the global range.
- One 20,000-character reply streamed at 200 characters/s on `Turn`'s flush
  schedule: 579 flushes, 1158 events, 10.1 MB of event payloads (502× the reply).

**Load scenario** (the scripted `claude` streams 20,000 characters over about
10 s; each client subscribes to the shell and the thread, on a short thread):

| Clients | server CPU | live event latency p50 / p99 | lag snapshots | events |
|---|---|---|---|---|
| 1 | 2.7 s | 1 / 3 ms | 0 | 261 rows, 1.4 MB |
| 10 | 3.2 s | 1 / 3 ms | 0 | 261 rows, 1.4 MB |
| 50 | 5.7 s | 6 / 24 ms | 0 | 261 rows, 1.4 MB |

On a short thread, client count alone is not the bottleneck. The cost grows
with thread length multiplied by subscribers (the store table above).

**Client core** (`view()` plus the bincode encoding `CoreFfi::view` sends, medians):

| Items | bytes per render | cold render | one streamed token |
|---|---|---|---|
| 100 | 0.29 MB | 3.5 ms | 0.63 ms |
| 500 | 1.46 MB | 17.9 ms | 3.9 ms |
| 2000 | 5.83 MB | 84.2 ms | 28.6 ms |

At 2000 items, one token already exceeds a 16 ms frame on this machine, before
the shell decodes 5.8 MB on its main thread.

**Lints** (`cargo clippy --workspace --all-targets -- -W clippy::pedantic
-W clippy::await_holding_lock -W clippy::unwrap_used -W clippy::redundant_clone`):
2490 warnings: server 1745, domovoi-core 553, the crates 192. Of these,
`unwrap_used` 1403 (tests included), `map_unwrap_or` 184,
`needless_pass_by_value` 75, `redundant_clone` 32, `too_many_lines` 18,
`struct_excessive_bools` 14, and `await_holding_lock` 0. That lint only covers
std guards; the orchestrator's lock is a tokio `Mutex`.

### After the performance work

Measured again at `0b7e1bf` with the same commands and machine:

| Measure | Before | After |
|---|---|---|
| Decode the session's client frames | 0.94 ms | 0.52 ms |
| Split the session's server frames | 4.48 ms | 1.86 ms |
| `shell()` at 5000 items | 9.21 ms | 1.07 ms |
| One streamed token, read model, 5000 items (apply + shell)¹ | 9.6 ms | 1.41 ms |
| Append one event | 0.10 ms | 0.06 ms |
| Flush + 10 subscribers' shells, 5000 entities | ~280 ms (10 × 28.2 ms + append) | 0.32 ms |
| `thread_events_after` behind 50,000 other events | 144 ms | 0.15 ms |
| Event payloads left by a 20,000-character reply | 10.1 MB | ~0.04 MB |
| Load scenario, 50 clients: server CPU / p99 latency | 5.7 s / 24 ms | 1.8 s / 7 ms |
| Client: one streamed token at 2000 items (`view()` + bincode) | 28.6 ms | 4.1 ms |
| Client: warm `view()` at 2000 items | 25.9 ms | 2.1 ms |
| Client: bytes per render at 2000 items | 5.83 MB | 5.83 MB (unchanged) |
| `unwrap()` outside tests | 149 | 0 (`just check` enforces it) |

¹ The first baseline (13.9 ms) also timed dropping the cloned thread; 9.6 ms is
the baseline code measured with the corrected bench.

The client still encodes the whole view model on every render: per-token
cost is now linear in thread length (0.9 ms at 500 rows, 4.1 ms at 2000)
rather than flat, and the shells decode those bytes, off the main thread
since `2c2d01e`. Sending only the parts that changed would change the
contract between the core and the shells and was left for a decision.

The load scenario's turn takes 12.4 to 14.6 s across runs of the same
build on a loaded machine. That time is the scripted provider's pacing,
and on reruns it matched the baseline's 12.5 s.

### Architecture work, first part

Measured 2026-11-16 on the same machine, against the numbers at `4b5137a`.

| Measure | Before | After |
|---|---|---|
| Bytes across FFI per streamed token, 2000 items | 5.83 MB | 273 B (a `ViewDelta`; the first view after start is still whole) |
| Core per streamed token at 2000 items (apply + view + encode) | 4.33 ms | 3.26 ms (`streaming_token_delta`) |
| Warm `view()` at 2000 items | 2.18 ms | 1.77 ms (target was 0.5 ms; not met: what is left is mostly `serde_json` map lookups, which typed records would remove) |
| Load scenario, 50 clients: server CPU / p99 | 1.5 s / 6 ms | 1.5 s / 5 ms |
| Append one event | 0.06 ms | 0.06 ms |

Correctness, each with a test that fails without the change:

- A command's events, outbox entries and receipt commit in one transaction
  (`Db::batch`, nested writes as savepoints, cache and broadcast after the
  commit). Without it, a receipt that fails to write left 14 events of a
  command that answered with an error.
- A thread's provider effects run in the order queued. Before, all three
  interrupts of a start/interrupt/start… sequence ran ahead of the starts.
- A scheduled task's run is a task of its own with an RAII reservation: a
  cancelled caller no longer leaves the task "already running" until restart.
- A client that stops reading holds up its streams instead of queueing in
  memory (bounded outbound queue, 256 messages). Unbounded, the test's stream
  produced 2.5 million values in 0.3 s.
- A stored record that does not decode is skipped with a warning, not read
  as `null`. `touch_thread` used to write `{ updatedAt }` over such a record.
  `latest_sequence` reports a failed read instead of answering 0.

Checked: `cargo test --workspace` (with `DOMOVOI_NODE`, so the t3code schema checks
ran), `just check`, `just android-test`, and `just mac-smoke` against a server
on a copy of `.reference/domovoi-home` (PASS). The delta path has its own replay test:
applying each delta reproduces `view()` after every frame of the recorded
session. Not checked: the iPhone app on a simulator or device, and moving
socket-frame handling off the shells' main threads (left for later).

### Architecture work, second part: the instruments, then what they showed

Measured 2026-11-16 on the same machine. "Before" is `6bac4c0` with only the
new measurements added.

The instruments were corrected first:

- **The schema checks skip under the default Node.** `just test` takes the
  `node` on `PATH`; here that is 22, and `golden_replay.rs` and `rpc_calls.rs`
  then print "schema check skipped" and pass. With `DOMOVOI_NODE` set to Node 26
  they decode every message and pass. Run with `-- --nocapture` to see which.
- **`socket_token`** (`cargo bench -p domovoi_core --bench view`) is one streamed
  flush as a shell pays for it: the socket frame through `update` (decode,
  apply, acknowledge), `FfiApp::view`, the delta's encoding, on a model booted
  from the recorded config (11 providers) with 300 threads in the sidebar.
  `streaming_token_delta` applied one event to an empty model and showed
  3.7 ms at 2000 rows; the whole path was 6.5 ms.
- **Server**, in `perf_tests.rs`: a flush through `Turn` with its commits
  counted, `message.dispatch` by thread length, four turns streaming at once,
  a flush with a shell subscriber, a snapshot of 500 threads, and the bytes a
  reply's deltas allocate. The older flush test wrote two events; a real
  flush is three (node, message, turn item).
- **`hot_queries_use_an_index`** traces every statement a thread's ordinary
  life runs and fails on a plan that scans a table. It reports its own
  unindexed control statement, so it can fail.
- **`DomovoiSmoke` measures main-thread stalls** (a timer on another queue posts
  to the main queue every 2 ms and records how late each post runs).
  `DOMOVOI_SMOKE_MAX_STALL_MS` makes a stall a failure.

A profile of `socket_token` at 2000 rows (`samply`) put the time in: the
composer's model lists, copied whole several times per render, about 40%;
`serde_json` map lookups in the thread page, about 30%; comparing rows for the
delta, nearly all of it diff hunks behind shared `Arc`s, about 19%; decoding
and applying the frame, about 1%.

| Measure | Before | After |
|---|---|---|
| Core, one streamed flush end to end, 500 / 2000 / 10,000 rows | 3.24 / 6.48 / 25.0 ms | 0.93 / 2.60 / 16.1 ms |
| Core, `streaming_token_delta` at 2000 rows | 3.68 ms | 1.99 ms |
| Warm `view()` at 2000 rows | 2.02 ms | 1.90 ms (the 0.5 ms target is still not met) |
| `message.dispatch`, thread of 100 / 1000 / 5000 entities (p50) | 0.65 / 3.94 / 29.3 ms | 0.33 / 0.37 / 0.48 ms |
| Commits per streamed flush | 3 | 1 |
| A flush while a shell subscriber reads the thread, 5000 entities (p50) | 2.87 ms | 0.19 ms |
| Shell snapshot of 500 threads | 63.8 ms on an async worker | 64 ms for the first on the blocking pool, about 1.5 ms after (14.0 ms mean of five) |
| Allocated by the deltas of a 20,000-character reply | 100 MB in 2.1 ms | 50 MB in 1.4 ms |
| Longest main-thread stall in `DomovoiSmoke` (release core) | 149 ms | not addressed yet |

What changed:

- **Core.** Every view type is `Eq`, so rows sharing a diff or markdown `Arc`
  compare by pointer. Model records are borrowed, not copied, when the
  composer lists them. An answer to a request the core no longer waits for
  returns no requests instead of panicking.
- **Commands read a thread's outline.** `Store::thread_outline` leaves out
  messages and turn items (nearly all of a long thread's bytes); a command
  that needs one message reads that message. Fork and handoff still read the
  whole thread.
- **A finished item searches only its own events.** Deleting its superseded
  flushes ran `json_extract` over every event of that type in the thread, on
  the writer, for each finished message and tool call. It now starts at the
  entity's first event. This was the larger half of the 29 ms.
- **A streamed update is one transaction**, and its three events reach the
  thread cache before subscribers hear of the first
  (`Db::after_commit_notify`). Written one by one, the subscriber woken by
  the first event was still reading the thread when the next was applied, and
  the cache copied the whole thread under the writer lock.
- **An uncached thread is loaded on a reader connection**, then catches up
  under the writer with the events committed meanwhile.
  `a_thread_loaded_while_events_land_misses_none` fails without the catch-up.
- **Shell snapshots run on the blocking pool** and remember each thread's
  shell until that thread's next event.

Measured and left alone:

- **Frame decoding** is 13.5 µs for a flush 20,000 characters into a reply,
  0.5% of the path.
- **The copy of the reply's text per delta** costs 2 ms over a whole
  20,000-character reply. The needless clone is gone; the copy the flush
  throttle keeps for the turn's end stays.
- **Writes stay on the async workers.** Four turns streaming at once queue for
  the writer (one flush: p50 0.14 ms alone, 0.72 ms with four), but a turn
  flushes every 120 ms, so the writer is busy well under 1% of the time.

Not done in this part: the child-process helper (it lands with its lint gate),
and the SwiftUI transcript's second pass over the rows (it moves with the
render path when the core leaves the main thread).

### Architecture work, third part: the core off the main thread

Measured 2026-11-16, same machine, `DomovoiSmoke` with a release build of the
core against a server on a copy of `.reference/domovoi-home`.

| Longest main-thread stall while | Before | After |
|---|---|---|
| connecting and receiving the shell | 39.9 ms | 0.5 ms |
| loading a thread | 23.7 ms | 0.0 ms |
| the remaining checks (settings, usage, terminal, search) | 149.2 ms | 0.2 ms |

Before, every socket frame was parsed and applied by the Rust core on the
main thread, effect requests were decoded there, the Keychain and cache files
were read and written there, and an event from the app waited for the view
being built on the render queue (the core keeps its model behind a lock).

- **Apple** (`Common/Core.swift`): `CoreEngine` owns the core on one serial
  queue. Events, effect answers and reading the view all run there, in the
  order sent. Socket frames go from URLSession's queue straight to it; the
  next frame is read once the core has applied the last. The store is
  answered on that queue. A burst of renders is read once (after what is
  queued, at most every 8 ms), and a main thread that is behind is handed
  the changes it missed merged into one. The transcript's lines are built
  with the page on that queue, where the view built them twice per token on
  the main thread.
- **Suspending** saves drafts and closes the socket before the iOS
  background task ends (`update(_:then:)`). On the simulator the task
  "Keep the connection" ended 26.1 s after it began (25 s of grace, then the
  suspend), in the same process.
- **Android** (`core/CoreLoop.kt`): the same on one thread, `domovoi-core`.
  `Core`'s effects, timers and state moved to it; what the app calls from the
  main thread is handed over. `CoreLoopTest` stands in for the Rust core:
  every call on one thread, events and their effects in the order sent, 500
  renders in a burst read at most 25 times with the last one shown, and an
  answer given from another thread refused.

Checked: `DomovoiSmoke` passes every check; the Mac and iPhone apps build; the
Android unit tests pass (18) and the app compiles. Not checked: the apps'
own drawing on the main thread (the smoke tool has no views), a device, and
the Android app on an emulator.

### Architecture work, fourth part: a streamed token that does not depend on the thread's length

Measured 2026-11-16, same machine, `cargo bench -p domovoi_core --bench view`
(`socket_token`: the frame through `update`, `FfiApp::view`, the delta's
encoding; medians).

| One streamed flush, by rows in the thread | At the start | After the second part | Now |
|---|---|---|---|
| 500 | 3.24 ms | 0.93 ms | 0.020 ms |
| 2000 | 6.48 ms | 2.60 ms | 0.020 ms |
| 10,000 | 25.0 ms | 16.1 ms | 0.021 ms |
| 2000, 20,000 characters into the reply | 6.51 ms | 2.70 ms | 0.040 ms |

The target was 0.5 ms at 2000 rows and no more than 1.3 times the 500-row
cost at 10,000; it is 0.02 ms and 1.03 times. The whole view built again
(any other change) is unchanged at 1.8 ms for 2000 rows: the 0.5 ms target
for that is still not met.

- **Each update says how far it reaches.** `DomovoiApp::update` counts every
  update as one that may change anything, except a socket frame whose
  replies are all events of the open thread that grow a row's text
  (`replies::reach`): an assistant message, thinking or a command's output,
  the same record again with only `text`, `markdown`, `output` and
  `updatedAt` different. Those name their row (`Model::changes`). A node's
  update reaches nothing: nodes are not shown.
- **`FfiApp::view` builds only those rows** when nothing wider happened since
  its last view: the item and its message are found from the thread's end,
  the row is built, what the page wrote on it from its place (search counts,
  step group, turn time) is kept, and the delta is that row. Anything else
  builds the whole view and compares, as before.
- **The check shares nothing with the fast path.** `FfiApp::checked()`
  compares every view made this way with the view built whole after clearing
  the row, markdown and diff caches, and panics on a difference.
  `deltas_applied_in_order…` runs with it (22 of 186 views of the recorded
  session took the fast path); `views_made_from_grown_rows_equal_views_built_whole`
  drives 600 generated steps (text growing, replies ending and starting,
  runs, a queued message edited, an approval asked and answered, a row
  expanded, growth together with a status) with views read at random
  (166 of 365 from rows). Taking one field out of what the row keeps
  (`group`) makes it fail.

Not done: the shells still copy the row list and compare its ids for each
changed row (on the core's queue on Apple, in composition on Android). Those
costs are not measured; nothing here can measure what the apps draw.

### Architecture work: child processes and queues

- **`process.rs` is where child processes start.** `process::command` kills
  its child when the task waiting for it is dropped; `process::output` also
  stops one still running after its limit (60 s for local work, 10 min for
  anything that reaches the network or runs hooks), and
  `process::output_blocking` does the same for code that is not async. Git
  for checkpoints, branch names, submodules, the pull-request and repository
  workflows, version probes and the provider status probe had neither; the
  submodule update ran under a thread's launch lock with no limit and could
  stop to ask for a password. Three tests: a child past its limit is stopped,
  output larger than a pipe comes whole, a child dies with its task.
- **`clippy.toml` disallows** `Command::new` outside `process.rs` and
  `mpsc::unbounded_channel`; `just check` denies both.
- **The seven unbounded queues stay**, each now saying why at its
  `#[expect]`. The plan meant to bound them; reading them showed the
  provider readers also deliver the answers their turns wait for, so a reader
  made to wait for room could never read the answer that frees it, and the
  effects queue is filled inside a transaction that cannot wait. The gate
  keeps a new one from appearing without a reason.

### Typed records, first step: the types, with nothing using them yet

`crates/domovoi-model/src/typed/` holds a Rust type for the thread, the thread
shell and each of the fifteen projection kinds, written from t3code's
`orchestrationV2.ts`: 24 id types (no conversion between kinds), 74 closed
string sets as enums that keep an unknown value as it came, optional and
nullable fields as the schema has them, and `TurnItem` as one record per
`type` with a fallback that keeps an unknown item whole. Server and core
still work on `serde_json::Value`; moving them is the next steps.

- `typed_round_trip` decodes every record in the two recorded
  TypeScript-server sessions and requires the same JSON back: 318 records of
  13 kinds and 7 turn item types, none through a fallback.
- The recordings have no subagents, plans, context handoffs or transfers,
  and 18 of the 25 turn item types never occur in them. Those are right by
  reading the schema, and by a check that is not in the repository: 19,160
  records generated from t3code's schemas (Effect's `Arbitrary`) went through
  the types unchanged.
- `stored_records_go_through_their_types` (ignored; needs `DOMOVOI_RECORDS`) does
  the same for a server's own database. On a copy of `.reference/domovoi-home`:
  70,676 records (685 threads, 34,498 messages, 34,593 turn items of ten
  types, the rest), 0 changed by the round trip.
- One spelling is normalised: t3code writes an optional field that is present
  but undefined as `null`; it is read as absent and written back absent.

### Typed records, second step: the thread itself

(As it stood at this step. The untyped model and `typed_state_agrees` were
removed at the end; see "the gate, and the old model removed" below.)

`domovoi_model::typed::Thread` is the read model on the types: the thread's
record and one collection per kind that keeps the order the untyped model
keeps and finds a record by id without searching. `apply` takes a typed
`DomainEvent` by value and says what it did (`Applied`: a record added or
replaced, whether the order changed, whether only its growing text
changed). The shell, the projection and the visible turn items are derived
from it with the rules `derive.rs` has, matched on enums. The untyped model
is unchanged beside it until the core and the server have moved.

It agrees with the untyped model wherever both were given the same input:

- **Recorded sessions**: 4 threads, 226 events; shell, projection and
  visible items equal after the snapshot and after every event
  (`typed_state_agrees`). All 226 events and 242 stream items also decode
  and encode unchanged (`typed_round_trip`); 29 of the schema's 46 event
  types never occur in the recordings.
- **Generated**: 1,200 steps by default (10,316 events with
  `DOMOVOI_GENERATED_STEPS=10000`) over new records of every kind, growing text,
  status flips, reordering, placeholder provider threads, detached sessions
  and unknown event types.
- **A real database** (ignored; `DOMOVOI_THREADS`, `DOMOVOI_EVENTS`): on a copy of
  `.reference/domovoi-home`, all 685 threads build to equal shells and
  projections, and all 72,430 stored events replayed from each thread's
  creation leave both models equal after every one.
- t3code's recorded shells (`parity.rs`) match the typed model too.

It is stricter on purpose: an event needs its id, thread and time; a record
missing a required field does not decode; an event type it does not know
changes no record, where the untyped model guessed from the type's prefix.

| `cargo bench -p domovoi-model`, 5000 turn items and messages | Untyped | Typed |
|---|---|---|
| Apply one streamed event (same thread, warm) | 0.81 µs | 0.14 µs |
| Apply it and derive the shell | 1.36 ms | 0.11 ms |
| Copy the whole thread | 6.24 ms | 0.046 ms |
| Apply while a reader holds the thread | 6.23 ms | 0.046 ms |

Decoding is where the types cost: an event already parsed to JSON takes
0.87 µs more to become a `DomainEvent`.

### Typed records, third step: the client core on the typed thread

`Model.thread` is `domovoi_model::typed::Thread`. Stream items are decoded with
`ThreadStreamItem::from_value`; the presenter matches on `TurnItem` variants
and enums; how far an event reaches comes from `Applied`. What crosses FFI
is unchanged, so the shells' bindings are too.

- **The pages are the same.** Before the change the old code's page for
  every thread of a real database was recorded (a hash per thread);
  `thread_pages_match_recorded_digests` (ignored; `DOMOVOI_THREADS`,
  `DOMOVOI_VIEW_DIGESTS`) rebuilds them with the typed code: 685 of 685 threads,
  the same 685 with every row expanded and a search open, and 506 frames of
  the two recorded sessions, 71,209 rows, 0 differences.
- **A row is rebuilt whenever a record it shows is replaced.** The row cache
  keys on the identity of the item and its message (their `Arc`s), where it
  hashed a few fields and could show a stale row when another field changed
  (`a_replaced_record_always_rebuilds_its_row`).
- **A stream item the app cannot read is left out and said**: the thread
  stays as it was, its position still advances, and a toast says what is
  shown may be incomplete. Before, anything was kept.
- The offline copy of the open thread keeps its format; one that no longer
  decodes is dropped quietly.

| `cargo bench -p domovoi_core --bench view`, 2000 rows | Before the types | Now |
|---|---|---|
| Warm `view()` | 1.80 ms | 0.52 ms (target 0.5 ms; what is left is copying and dropping the rows' strings, which cross FFI) |
| A change that is not a reply's text growing (whole view, compared, encoded) | 1.88 ms | 0.60 ms |
| One streamed flush (`socket_token`) | 0.020 ms | 0.020 ms, the same at 10,000 rows |

### Typed records, fourth step: the server's event path

The store, the thread cache, the streams and `Turn` work on the types.

- **`Turn` builds records with struct literals**, and what it appends is a
  typed `EventBody` (`Store::append_event`). The command handlers and the
  other modules still build JSON; their `append_thread_event` decodes it into
  the event's type and refuses, writing nothing, an event the schema does
  not allow. Across the unit and integration suites the server built none.
- **An event is serialized once.** The same text is the log's row and, from
  its `payload`, the projection's row. Moving the thread's `updatedAt` is
  one `json_set`; the record is no longer read, parsed and rewritten per
  event.
- **The cache holds `Arc<typed::Thread>`**: applying an event while a reader
  holds the thread copies pointers (0.05 ms at 5000 records), where it
  copied the thread (6 ms).
- **Codex wrote an edit's patch as `diff`**, a field the schema does not
  have; it is `diffStr` now, and a stored record or event with `diff` is
  read with it under `diffStr` (`stored.rs`).
- **What clients receive is the same.** `stored_threads_are_sent_as_before`
  (ignored; `DOMOVOI_SERVER_HOME`, `DOMOVOI_SERVER_DIGESTS`) compares, for every
  thread of a real database, its shell in the shell snapshot, its thread
  snapshot and its replay from the first event with what the code before
  sent: 685 of 685 equal as JSON. The order of keys within objects differs
  everywhere (struct order, not `json!` order).

| Server, same machine | Before the types | Now |
|---|---|---|
| `message.dispatch`, thread of 100 / 1000 / 5000 entities (p50) | 0.33 / 0.37 / 0.48 ms | 0.25 / 0.25 / 0.27 ms |
| A thread's shell read from SQL, 5000 entities | 24.5 ms | 11.0 ms |
| Shell snapshot of 500 threads (mean of five) | 14.0 ms | 8.5 ms |
| Append one event | 0.06 ms | 0.045 ms |

A streamed flush through `Turn` read 0.5 to 1.0 ms in both binaries on this
day's loaded machine; run back to back three times the typed one was the
faster each time (0.81 / 0.58 / 0.55 ms against 1.03 / 0.97 / 0.73 ms at 100
/ 1000 / 5000 entities in the first pair).

End to end: the Mac and iPhone apps build on the typed core, `DomovoiSmoke`
passes against the typed server with the stall limit (longest 1.0 ms), the
Android unit tests pass, and `cargo test --workspace` with the schema checks
running passes (408 tests).

Still JSON at the end of this step, and typed in the steps that follow: the
command handlers, the other callers of the store, the RPC groups, and in the
core everything but the open thread.

### Typed records, fifth step: commands

`Command` (`domovoi-model/src/typed/command.rs`) has a variant for each of the 71
command types of t3code's schema (56 a client may send, 15 the server sends
itself) and one for a type this build does not know. A command is decoded
once, where the RPC arrives; `Orchestrator::dispatch` takes a `Command` and
answers `Result<DispatchCommandResult, DispatchFailure>`. The 1,987 lines of
`commands.rs` are `orchestration/commands/` by family: dispatch and locks,
failures, launch, messages and steering, thread fields, the queue, runtime
requests, fork and merge-back.

- **Checked by a differential.** `command_differential` (ignored;
  `DOMOVOI_DIFFERENTIAL_OUT`) runs 192 commands and launches through the
  orchestrator, every type the handlers support and each refusal, and dumps
  every result, error, event (386), effect, receipt and final record. The
  dump of the code before and of the code now are equal as JSON, at this
  step and after each later one. Removing one field from one handler made
  it differ.
- **Recorded traffic passes through.** The 33 commands (5 types) and 3
  launches in the recordings decode and encode back unchanged. The other 66
  types are written from the schema alone; an example of each public type
  decodes under t3code's own schema on Node 26.
- **What changed for a client.** A command missing a field the schema
  requires, or with a `dispatchMode` or `sourcePoint` of an unknown type, is
  refused where it arrives ("Command X is not one domovoi-server can read: ..."),
  where a handler used to take a default. Fork and merge-back lock the thread
  they act on. Five fields t3code requires stay optional here
  (`thread.unsettle.reason`, `thread.unsnooze.reason`, `subagent.stop.runId`,
  `run.interrupt.runId`, `thread.auto-settle.snapshotAt`): the first two the
  core now always sends, and could be required.

Run back to back with the binary from before the step: `thread.visit`
69-125 -> 66-86 µs and `message.dispatch` 258-364 -> 191-232 µs (p50 over
three runs each), the decode of the JSON included.

### Typed records, sixth step: the rest of the client core

Everything the core holds from a server is a record, and everything it sends
is built from one.

- **State.** The thread list is `typed::ThreadShell`; settings, server
  configuration, providers, keybindings, environments, projects, version
  control, pull requests and the settings pages have records in `typed/`
  (`settings.rs`, `server.rs`, `keybinding.rs`, `environment.rs`,
  `project.rs`, `workspace.rs`, `vcs.rs`, `source_control.rs`, `pages.rs`,
  `remote_access.rs`, `list.rs`), written from the contracts. A reply or
  stream item is decoded once, in `replies.rs`. One that fits no record is
  left out, counted and said in a toast once per kind; it used to be shown
  with blanks.
- **Sending.** A command is a `typed::Command` struct literal, the same type
  the server decodes with. Every other request is built by one function per
  RPC method in `requests.rs` (60 methods, 5 HTTP bodies).
- **Checked by digests of the whole view.** `tests/view_digests.rs`
  (ignored; `DOMOVOI_THREADS`, `DOMOVOI_WALK_DIGESTS`) walks 1,396 steps over both
  recordings and 685 real threads in 42 projects (lists, filters, search,
  palette, settings pages, model picker, environment switch, the offline
  copy) and compares each encoded view, every frame sent (1,426) and the
  offline copies saved with those of the code before: all equal. Uppercasing
  one provider status made 927 steps differ.
  `every_request_the_core_sends_is_as_recorded` reaches each of the 122 send
  sites (151 steps, 201 frames, recorded from the code before).
- **Three payloads differ, by choice:** `thread.unsettle` now says
  `"reason": "user"`, and `subagent.stop` leaves `runId` out rather than
  sending `null` in the two cases with no run.
- **Every command the core sends decodes** with `typed::Command` to a known
  variant and encodes back equal (61 in the recording, 92 in the walk, each
  send of the 78 replay tests): the server's stricter edge refuses none.
- **Recorded traffic passes through** the new records unchanged: 6 configs,
  9 settings documents, 262 providers, 528 keybindings, 30 environments, 27
  projects, 62 thread shells. Pull requests, git actions, scheduled tasks,
  usage, diagnostics and the other pages appear in no recording and are
  typed from the contracts and the server's code.
- **The offline copy** an earlier build saved still loads and gives the same
  views; one with a record that does not read is ignored whole.

The view of the 685-thread list takes 0.36 ms where it took 27.5 ms (the
project sort rescanned every thread per comparison). The benches of the open
thread did not move: `socket_token` 20.7 -> 20.6 µs, `warm_view_only` at 2000
rows 483 -> 497 µs (2 to 3% slower in repeated pairs, level with one codegen
unit, so not from the view code, which did not change).

### Typed records, seventh step: the rest of the server

- **The store has no untyped functions.** `append_thread_event`,
  `update_thread_event`, `thread_record(s)`, `entity`, `entities_of_kind` and
  `thread_entities` are gone with their 140 call sites; recovery, repair,
  effects, checkpoints, rollback, naming, session import, MCP, push and the
  adapters read and write records.
- **Every RPC group answers with records** and fails with a `thiserror` enum
  that serializes to the tagged JSON t3code defines. The records are the ones
  the core decodes with, so server and client agree at compile time. No
  record needed correcting for it. The project row's identity, model
  selection, icon and scripts are records too; a row whose field does not fit
  reads with that field unset rather than dropping the project (the 43 rows
  of the real database all fit).
- **Provider output stays JSON inside the functions that parse it** (Claude
  stream-json, Codex JSON-RPC, OpenCode, `gh`, other tools' transcripts, the
  MCP wire, APNs): the rule is that `Value` may describe someone else's
  format or a field the schema leaves open, never one of ours.
- **Settings keep unknown keys.** t3code decodes the file with its schema and
  drops what it does not know on the next save; Domovoi kept such keys before and
  still does (`settings::Settings` is the record plus what it could not
  read).

Checked by dumps taken before and after:

| Dump | Size | Result |
|---|---|---|
| `command_differential` | 192 commands, 386 events | equal |
| Record dumps (`record_dumps.rs`; `DOMOVOI_DUMP_DIR`): recovery, repair, session import, checkpoints and rollback | 1,536 events | equal |
| Turn dumps (`provider_turns.rs`; `DOMOVOI_TURNS_DUMP_DIR`): 20 scripted turns of Claude, Codex and OpenCode against the real binary, and the answers of `rpc_calls` | 1,715 events and answers | equal; three equal record by record (event order between records varies run to run) |
| Stored threads as sent (`stored_threads_are_sent_as_before`) | 685 threads of a real database | 686 of 686 equal |
| RPC groups (`conformance/rpc-groups`, 46 files) | 202 methods, 2,199 answers of which 804 failures | equal but for `scanDurationMs`, a clock reading |

The RPC groups call each method for every answer and failure it can give, on
a server of their own per group with stand-ins for `gh`, `ssh` and the
provider CLIs. They take six to ten minutes, so they are not part of
`cargo test`: `just rpc-groups` runs them (`just rpc-groups vcs-status` one
group). On this machine, loaded by other work at the time (load average 13
to 28), two of four full runs failed on a reply that took longer than the
test's 15 s; each affected group passed alone. That is not explained
further.

Codex has a scripted integration test now
(`a_scripted_codex_turn_records_every_kind_of_item`: every item kind, six
request kinds, a failed turn, steer, interrupt, revert) against a stand-in
app-server, not the real one; `thread/resume` and `thread/fork` are not in
it.

Three faults the groups pinned, which predate this work, are fixed:

- `projects.writeFile` could not replace a file that exists (the path of an
  existing target was built with a trailing separator).
- `vcs.refreshStatus` for a folder that is no repository left out
  `hasUpstream`, `aheadCount`, `behindCount` and `pr`, which the schema and
  now the core require; it answers them as none.
- The `schedule_task` MCP tool could never save a task (it gave no runtime or
  interaction mode); a task takes those of the thread that schedules it.

Answers that are outside t3code's schema and were left as they are (each a
`skip_schema` case with its note): the `shell.openInEditor` failure has no
`target`; `assets.createUrl` fails with `AssetUrlError` and `terminal.open` /
`terminal.attach` with `TerminalCwdError`, tags t3code does not have; pull
request detail, preview and activity carry blank strings where the host
reports nothing; `sourceControl.lookupRepository` sends empty fields `gh`
left out; `preview.list` returns the colour scheme it was given.

Counts in `apps/server/src`, at the start of the fifth step and now:
`Result<_, Value>` 182 -> 3 (Codex's JSON-RPC result, and two in the
differential's harness); `json!` outside tests about 1,270 -> 147, all
writing a foreign format.

### Typed records: the gate, and the old model removed

- **`just check` refuses JSON held as JSON** (`clippy.toml`:
  `disallowed-types` for `serde_json::Value` and `Map`, `disallowed-macros`
  for `json!`) outside tests, except in a module that says with
  `#![expect(..., reason = "...")]` whose format it reads: 31 modules of the
  server, 5 of the core (`rpc.rs`, `replies.rs`, `requests.rs`,
  `offline.rs`, and `present.rs` for fields the schema leaves open),
  `domovoi-contracts`' bundled defaults, and three crates whole (`effect-rpc`,
  the framing; `domovoi-model`, where JSON becomes records; `ws-tap`, a
  recorder). A reason that no longer holds fails the gate too
  (`unfulfilled_lint_expectations`). A `Value` added to
  `orchestration/stream.rs` or to the core's `update.rs` fails it.
- **The untyped model is deleted** from `domovoi-model` (`ThreadState`,
  `derive.rs`, `notify.rs`: 641 lines to 66 at the crate root). The test
  that compared the two models is `tests/typed_thread.rs`, which asserts the
  rules directly on the same recorded and generated events (226 and 1,250).

Performance at the end, against the binaries from before the fifth step, in
alternation on the loaded machine (so the absolute figures read about twice
those above, in both columns):

| Server (three runs each) | Before | Now |
|---|---|---|
| `message.dispatch` p50, 5000 entities | 657 / 744 / 778 µs | 386 / 416 / 468 µs |
| `thread.visit` p50, 5000 entities | 170 / 203 / 216 µs | 134 / 135 / 143 µs |
| Streamed flush through `Turn`, 5000 entities | 436 / 471 / 481 µs | 437 / 442 / 503 µs |
| A thread's shell read from SQL, 5000 entities (six runs, median) | 21.5 ms | 22.7 ms |
| Shell snapshot of 500 threads | 14 / 16 / 18 ms | 12 / 14 / 16 ms |

The core's `socket_token` read 28 to 34 µs with the bench binary from before
the sixth step and 30 to 38 µs with this one in the same alternation: level
within the noise of that day.

One thing the final runs turned up that is not about types: `remote_access`,
`golden_replay` and `pairing_permissions` started their servers with the
developer's own agent home, so each server indexed the real `~/.claude` logs
for the usage page as it started (9.1 GB that day, in a debug build, six
servers at once in `remote_access`), and replies over the tunnel missed the
test's ten seconds: three to five of five runs failed with the real home,
five of five passed with an empty one. They now set
`DOMOVOI_AGENT_SESSIONS_HOME`, as `rpc_calls` already did.

At the end: `just check` passes; `cargo test --workspace` with the schema
checks running (`golden_replay` 156 messages, `rpc_calls` 630 messages of 219
methods, the command examples 66) had 555 tests pass and one,
`a_scripted_opencode_turn_records_every_kind_of_item`, miss a 15 s wait on
the loaded machine (its test binary then passed three runs of three); the
Mac and iPhone apps build on the new core; `DomovoiSmoke` passes against the
server on a copy of a real home (longest main-thread stall 1.2 ms); the
Android unit tests pass.

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
    (zig, GNU target); not run on Windows. Linux: `just check-linux` builds and
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
  - Since then (see "Agent tools and instructions" below): 27 tools, with
    t3code's names and inputs. Not done: t3code's preview, device, HTML,
    attachment, PR-link, environment-link and secret toolkits.
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
  - Not done: OpenCode forms (questions are cancelled so the turn doesn't
    hang), native fork and rollback, subagents, and usage limits.
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

- `apps/server/src/mcp/` serves 27 tools: threads (`t3_thread_list`,
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
  to Pi through its extension's system prompt. Not done: OpenCode gets neither
  the tools nor the instructions (Domovoi drives OpenCode's v2 API, whose MCP
  and system-prompt calls are unverified).
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
- Not done: reading limits before a turn reports them (Claude `get_usage`,
  Codex `account/rateLimits/read`), reset credits
  (`provider.consumeResetCredit` still answers `noCredit`), and the
  system notice t3code shows in the timeline when a limit is hit.
- Verified: unit tests for both providers' messages as they send them;
  `provider_turns.rs` `a_usage_limited_thread_resumes_when_the_limit_resets`
  (a scripted Claude turned away; the windows on the provider; a wrong run
  refused; the continuation). Not live: no real limit was hit on purpose.

## ACP agents

- `adapters/acp.rs`: any Agent Client Protocol agent, as a provider
  instance of driver `acpRegistry` (t3code's) whose config names a local
  command (`source: "local"`, `commandPath`, `commandArgs`, and the
  instance's `environment`) or a registry agent (`agentId`). One process per
  thread: `initialize` (protocol 1; no client file system or terminal),
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
  prompt of a session carries the instructions about them. No steering
  (ACP has none): a message sent while a turn runs waits for it.
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
  with t3code's typed `*_unsupported` reasons; `acceptAcpRegistryUrlAuth`
  accepts nothing (no agent asks for URL authentication). Grok and Kiro run
  as the registry's `grok-build` and `kiro` agents (t3code's own flavours of
  them are not ported).
- Verified: unit tests for the search (what runs here, ids, icons) and a
  binary install with its checksum (and a wrong one refused); the
  `rpc-calls` and `admin-server` cases search a fixture registry
  (`conformance/acp-registry.json`, never the network), and their answers
  decode with t3code's schemas. Live: `DOMOVOI_LIVE_ACP_REGISTRY_AGENT=pi-acp
  cargo test -p domovoi-server --test provider_turns live::a_registry_agent`
  finds the 46 agents of the real registry, installs `pi-acp` through npm,
  and its instance starts and is ready.

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

