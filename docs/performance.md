# Performance

How a change's effect on speed and size is measured, and the measurements of
each change since the compatibility layer with t3code began to come out
(the ledger). What was measured before that is under [History](#history).

## How a change is measured

A single run on this project's development machine means little: the machine
is busy with other work, and the same build's numbers differ by a factor of
two from one hour to the next. So two builds are always compared by running
them in alternation, several times, and reading medians.

```sh
just perf-save before            # build this checkout's measurements and keep them
# ... make the change ...
just perf-save after
just perf-pairs before after     # run both in alternation (three pairs), then print the table
just perf-compare --self before  # one build against itself: the noise right now
```

`tools/perf.py save <label>` copies the checkout's own files to
`.reference/perf-build` (a second tree with its own target folder, so the
checkout stays free to edit and build), builds the benches, the `perf_*`
tests and the server there in release mode, and keeps the executables and
the `fixtures` folder they read in `.reference/perf/<label>/`. A kept build
no longer depends on the checkout: it can be run after the code has changed.

`run` records one JSON file of measures per run. `compare` prints, for each
measure, the two medians, the change, and the larger of the two builds'
spread (half the range over the median; at least 20% for a time, the noise
floor below; nothing for bytes and counts, which are exact). A change inside
the spread reads as "same". Every measure is a cost: lower is better.

### What is measured

| Group | Measures | Where |
|---|---|---|
| `frames.*` | A recorded session's frames: decoding the client's, encoding and decoding the server's, as JSON and as MessagePack, and what zstd adds; the session's bytes in each encoding | `crates/domovoi-rpc/benches/frames.rs` |
| `thread.typed_*` | The thread read model at 100/1000/5000 items: its shell, applying one event, one streamed token, decoding an event | `crates/domovoi-model/benches/thread.rs` |
| `view.*` | The client core at 100/500/2000 items: a cold render, a warm `view()`, one streamed token, a socket frame through `update`, opening a thread from its snapshot frame | `apps/domovoi-core/benches/view.rs` |
| `store.*`, `turn.*`, `command.*`, `launch.*`, `shell.*` | The server's store and turn code on a file-backed database: appending, flushing a streamed reply through `Turn`, command latency by thread size, resuming behind other threads' events, shell snapshots, bytes allocated by streamed deltas | `apps/server/src/perf_tests.rs` |
| `thread.snapshot.*` | A thread's snapshot at 100/1000/5000 items: time to build and to encode, and its bytes on the wire | `perf_tests.rs` |
| `turn.reply_20k.*` | A 20,000-character reply streamed through `Turn` over ten seconds: bytes sent to one subscriber, events, what the reply leaves in the database | `perf_tests.rs` |
| `load.<clients>.*` | The scripted provider streams 20,000 characters while 1, 10 and 50 clients watch: server CPU, live event latency, lag snapshots, the server's peak memory | `apps/server/tests/provider_turns.rs` |
| `connect.*`, `call.*` | A signed-in device opening its socket, getting the shell and a thread snapshot; one small call there and back | `provider_turns.rs` |
| `real.*` | On a copy of a real home (`.reference/perf-home`, copied again before it is opened): opening the database, the shell snapshot, the largest thread's snapshot, reading every thread. No server is started on it: one would run the home's scheduled tasks | `perf_tests.rs` (`DOMOVOI_PERF_HOME`) |
| `size.*`, `code.*` | The server binary's size; lines of Rust; files opting out of the JSON-value ban; lines mentioning t3code | read off the checkout at `save` |

When the fixtures or a bench's subject change, the old and the new are run
once on the same build and that row is written down, so a new input does not
read as a change of speed.

### The data migration

Each step of a records rewrite (`apps/server/src/data_format.rs`) is timed and
logged at the server's start: the copy made first, then every step. It is
tried on a fresh copy of `.reference/perf-home`, never on the home itself, with
`domovoi-server doctor --records` before and after.

## Ledger

Each entry compares two kept builds run in alternation, three runs each,
on the development machine (an Apple M-series laptop, busy with other work:
load 4 to 6). All builds are release builds. The copy of a real home the
`real.*` rows use holds 1,027 threads and 153,723 records.

### M0: the baseline, and the noise (`9ee91e3`)

The baseline against itself (`perf-compare --self m0`, two runs against two)
puts every time within 20% but these, which jump between two values from
one run to the next:

| Measure | Spread |
|---|---|
| `command.thread_visit.1000.p50` | ±26% |
| `load.10.latency_p99`, `load.50.latency_p99` | ±21%, ±23% |
| `real.largest_thread.snapshot.build` | ±24% |
| `thread.typed_apply_while_read.5000`, `thread.typed_clone_state.1000`, `.5000` | ±41%, ±56%, ±40% |

So a time that moves by less than 20% is no change, and the measures above
are not evidence of anything by themselves. Bytes and counts do not vary.

| Measure | M0 |
|---|---|
| The largest real thread's snapshot | 16.66 MB, 9.1 ms to encode |
| The real home's database | 1,125 MB |
| A thread's snapshot at 100 / 1000 / 5000 items | 0.21 / 2.10 / 10.50 MB |
| A 20,000-character reply to one subscriber | 1.47 MB in 224 events |
| What that reply leaves in the event log | 104 kB (1.41 MB at its peak) |
| Bytes allocated by its 5000 deltas | 50 MB |
| One flush of a streamed reply | 0.51 ms, 3 events, 1 commit |
| A recorded session's server frames | 3.79 MB as JSON, 3.48 MB as MessagePack, 1.00 MB with zstd |
| Server CPU while 1 / 10 / 50 clients watch a reply | 0.7 / 0.8 / 1.2 s |
| Server peak memory for the same | 53 / 59 / 71 MB |
| One small call there and back | 83 µs |
| Server binary | 61.6 MB |
| Lines of Rust; lines mentioning t3code | 193,504; 1,303 |

### M1: the tether to t3code cut (`9ee91e3` → `ffe88f7`)

Expected: smaller, and no change of speed. That is what was measured.

| Measure | M0 | M1 | |
|---|---|---|---|
| Lines of Rust | 193,504 | 191,210 | -1.2% |
| Lines mentioning t3code | 1,303 | 1,113 | -14.6% |
| Server binary | 61.61 MB | 61.40 MB | -0.3% |
| Registered RPC methods | 243 | 198 | 45 removed |
| Every `store.*`, `turn.*`, `command.*`, `connect.*`, `call.*`, `view.*`, `real.*` time | | | within the noise |
| `load.1.latency_p99` | 2.8 ms | 5.1 ms | one of the measures that jump (±60% in these runs) |

The recorded session changed with this milestone (from t3code's own server
to `fixtures/native/session1`), so the `frames.*` rows and the thread and
view benches that replay the recording do not compare across it. The bridge:
the M1 frames bench run once on the M0 recording gives M0's numbers
(decoding the client's frames 429 µs against 445 µs, decoding the server's
10.0 ms against 10.0 ms, encoding them 2.90 ms against 2.92 ms).

### M2: a thread sent once, a streaming reply sent as the text added (`ffe88f7` → `09713dc`)

Expected: snapshots about half the size, a streamed reply's bytes linear in
its length, cheaper flushes, less server CPU under load, a much smaller
database. All of it held.

| Measure | M1 | M2 | |
|---|---|---|---|
| The largest real thread's snapshot | 16.66 MB | 8.32 MB | -50% |
| … encoding it | 9.32 ms | 4.44 ms | -52% |
| … building it a second time (now from the thread cache) | 2.81 ms | 0.02 ms | -99% |
| A thread's snapshot at 100 / 1000 / 5000 items | 0.21 / 2.10 / 10.50 MB | 0.14 / 1.35 / 6.73 MB | -36% |
| … building it | 0.23 / 1.51 / 10.9 ms | 0.13 / 0.50 / 2.4 ms | -43% to -79% |
| … encoding it | 0.31 / 2.91 / 16.2 ms | 0.19 / 1.77 / 9.6 ms | -38% to -41% |
| A 20,000-character reply to one subscriber | 1.39 MB in 213 events | 76 kB in 70 events | -94.5% |
| What it leaves in the event log | 101 kB (1.33 MB at its peak) | 43 kB (0.62 MB) | -57% |
| Bytes allocated by its 5000 deltas | 50.06 MB | 66 kB | -99.9% |
| … and their time | 1.25 ms | 0.22 ms | -82% |
| One flush of a streamed reply | 0.47 ms, 3 events | 0.33 ms, 1.05 events | -29% |
| Server CPU while 1 / 10 / 50 clients watch a reply | 0.7 / 0.8 / 1.0 s | 0.5 / 0.6 / 0.8 s | -22% |
| Server peak memory for the same | 51 / 56 / 69 MB | 45 / 49 / 59 MB | -12% to -14% |
| The client core taking one streamed token off the socket | 19.0 µs | 8.9 µs | -53% |
| … one flush frame | 11.5 µs | 1.3 µs | -88% |
| The client core opening a 2000-item thread from its frame | 96.4 ms, 4.06 MB | 83.0 ms, 2.17 MB | bytes -46%; the time is within the noise |
| The real home's database | 1,125 MB | 418 MB | -63% |
| Command latency, connecting, the shell snapshot, a cold render | | | within the noise |

The data migration (format 1 to 2) on the copy of the real home: the backup
copy 1.97 s, trimming the event log and compacting the file 2.65 s, 4.69 s
in all; all 153,723 records decode before and after
(`domovoi-server doctor --records`).

Worse, and why it is kept:

- `real.open` 0.50 ms → 0.85 ms: opening the database now reads its data
  format too. A third of a millisecond, once per start.
- `command.thread_visit.1000.p50` 52 µs → 80 µs and
  `command.message_dispatch.1000.p50` 178 µs → 219 µs: the first is one of
  the measures that jump between these two values (M0 measured 83 µs, M1
  79 µs and then 52 µs); the same commands at 100 and 5000 items did not
  move. Counted as noise.
- `frames.largest_server_message` 49 µs → 65 µs: the recording changed again
  (it now holds what a client is sent), and with it which message is the
  largest. The other `frames.*` rows are smaller for the same reason and
  are not a speed-up of the codec.

Not measured: the thread bench (`thread.typed_*`) of the M2 build. Its
start-up check counted the records of the old recording and stopped the
bench; the check was corrected with M4, whose entry has those rows.

What the codec measurements say, which decided M4: the recorded session is
457 kB as JSON and 401 kB as MessagePack (12% less), 132 kB against 130 kB
once compressed; MessagePack encodes faster (0.25 ms against 0.39 ms for the
session) and decodes no faster (1.85 ms against 1.69 ms).

## History

Measured before the ledger above, with single runs on a quieter machine. The
recorded session then was one of the t3code TypeScript server
(`fixtures/golden/ts-session1`, since removed), so the wire and client numbers
below are not comparable with the ledger's.

### The first baseline (2026-11-16)

Measured 2026-11-16 at `dc22ef9` on an Apple M5 Max, optimized builds. These
are the reference numbers for the performance work; rerun the same commands
after a change and compare.

```sh
cargo bench -p domovoi-rpc --bench frames        # wire codec over a recorded session
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

#### After the performance work

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

#### Architecture work, first part

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

#### Architecture work, second part: the instruments, then what they showed

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

#### Architecture work, third part: the core off the main thread

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

#### Architecture work, fourth part: a streamed token that does not depend on the thread's length

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

#### Architecture work: child processes and queues

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

#### Typed records, first step: the types, with nothing using them yet

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

#### Typed records, second step: the thread itself

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

#### Typed records, third step: the client core on the typed thread

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

#### Typed records, fourth step: the server's event path

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

#### Typed records, fifth step: commands

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

#### Typed records, sixth step: the rest of the client core

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

#### Typed records, seventh step: the rest of the server

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

#### Typed records: the gate, and the old model removed

- **`just check` refuses JSON held as JSON** (`clippy.toml`:
  `disallowed-types` for `serde_json::Value` and `Map`, `disallowed-macros`
  for `json!`) outside tests, except in a module that says with
  `#![expect(..., reason = "...")]` whose format it reads: 31 modules of the
  server, 5 of the core (`rpc.rs`, `replies.rs`, `requests.rs`,
  `offline.rs`, and `present.rs` for fields the schema leaves open),
  `domovoi-contracts`' bundled defaults, and three crates whole (`domovoi-rpc`,
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

