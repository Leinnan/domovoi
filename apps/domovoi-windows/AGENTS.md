# domovoi-windows: the WinUI 3 app

A pure-Rust WinUI 3 shell on `windows-reactor`, using the fork lwa_fm builds
on and pinned in the root `Cargo.toml`. The client core is linked directly.
The host (`crates/domovoi-desktop-host`) runs its effects: HTTP, WebSocket,
the iroh tunnel, the store and notifications. The shell only draws and
collects input. Off Windows the crate builds an empty binary, so
workspace-wide commands still pass. `docs/windows.md` describes the app, its
milestones and its packaging.

## Modules

| Module | What |
|---|---|
| `main.rs` | Single instance and `domovoi://` hand-off (`links::start`), the scheme and AppUserModelID, then `App::run_component::<MainWindow>` |
| `launch.rs` | Arguments (a link) and `DOMOVOI_SERVER`, `DOMOVOI_PAIRING`, `DOMOVOI_SMOKE` (`just windows-smoke`) |
| `core_link.rs` | A Tokio runtime with the host on it. Host events go into an inbox, and each one wakes the UI thread. Also the 5 s `Tick`, and links from later launches |
| `ui_thread.rs` | A message-only window: `wake()` from any thread runs the installed callback on the UI thread, and wake-ups are folded into one |
| `store.rs` | The `ViewModel`, kept current with `ViewDelta`s, split into `Rc` parts. Only the parts a delta names are replaced |
| `window.rs` | `MainWindow`: absorbs host events, schedules `RetryTimer`, tells the host the thread on screen, and lays out the title bar, pairing or sidebar, thread and composer |
| `views/pairing.rs` | Pairing link, or server URL and code |
| `views/sidebar.rs` | Threads by status section or by project; the server's name and state; New thread |
| `views/thread.rs` | The transcript (markdown blocks, commands with their output's end, edits, tool calls), pending sends, approvals |
| `views/composer.rs` | The draft (the window's own copy, see below), Send (Ctrl+Enter) and Stop |
| `theme.rs` | Sizes, brushes and the icon font |

## Rules (learned in lwa_fm and wihaister)

- Reactor messages can only be sent from the UI thread. Other threads go
  through `ui_thread::wake`.
- Never block on a WinRT async operation on the UI thread. Before the first
  window that leaves a live process with no window.
- Anything with a message loop of its own (pickers, menus, drags) starts
  after the current callback has returned, never inside one.
- Build window-level callbacks once (in `create`), not in `view`. Reactor
  replays some window events on each publish, and a new callback each time can
  loop.
- A text box whose text is set from the core loses its caret on each echo.
  Show the window's own copy, and take the core's only when it changed the
  draft itself (`ComposerView::draft_revision`).
- `cargo` stages the Windows App Runtime beside the exe from `build.rs`
  (`windows_reactor_setup::as_self_contained`). A warm cache can skip copying
  it, so the packaging script clears what an earlier build staged beside the
  exe and touches `build.rs` first.

## Tests

`cargo test -p domovoi-windows` runs the unit tests (store splitting, launch
parsing, sidebar and transcript helpers). `just windows-smoke` starts the app
against a running server with a fresh pairing code, and fails unless it
connects. The packaging script checks that the installed app opens its window.
