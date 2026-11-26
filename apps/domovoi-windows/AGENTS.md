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
| `core_link.rs` | A Tokio runtime with the host on it. Host events go into an inbox, and each one wakes the UI thread. Also the 5 s `Tick` (which tells the core when the machine slept or its network changed), links from later launches (`OpenLink`), and pictures read for a message away from the UI thread (`attach`) |
| `hwnd.rs` | The native window, subclassed: when it gains and loses the focus (`AppFocus`, and no notification for the thread on screen) and when it closes (`shutdown`) |
| `shortcuts.rs` | Keys to commands: new thread, Settings, the sidebar, stop, find, the palette (Ctrl+K, Ctrl+P, Ctrl+Shift+F), the panel (Ctrl+J) and its tabs, Escape. Which of them still work while the terminal has the keyboard |
| `companion.rs` | The server's small face: its mood from what the server does, its moment of gladness or dismay when a turn ends, and its drawing (an SVG made once for each look and mood) |
| `time.rs` | A moment as this machine's clock shows it |
| `menus.rs` | What a menu choice (`t-pin:<thread id>`) tells the core, where that is all it does. The window's `menu` has the choices that open a dialog |
| `ui_thread.rs` | A message-only window: `wake()` from any thread runs the installed callback on the UI thread, and wake-ups are folded into one |
| `store.rs` | The view model's sections, kept current with `ViewDelta`s, each an `Rc`. Only the sections a delta names are replaced; of the open thread only the rows that changed (`ThreadPatch`) |
| `media.rs` | The server's pictures (project icons, the icon picker's thumbnails, a thread's pictures and diagrams, a picture file in the panel): asks the core for links (`LoadProjectIcon`), fetches them through the host, keeps them by URL |
| `icons.rs` | The symbols a project icon can be, and a few drawings of the window's own: the Iced app's Lucide drawings, coloured |
| `widgets.rs` | What more than one view draws: a project's icon tile, the toast, a file's change, highlighted code (`code_lines`), a provider account's usage windows (`limit_windows`), a line drawing (`line_icon`) |
| `overlays.rs` | The window's one dialog and what it asks (`Overlay`): folders, names, sessions to import, a project's icon, a server to add, a provider account, a scheduled task, confirmations |
| `window.rs` | `MainWindow`: absorbs host events, schedules `RetryTimer`, tells the host the thread on screen, keeps the preferences, routes menu choices (`menu`) and what the views report (`transcript`, `panel`), and lays out the title bar, then pairing, Settings, or the sidebar with a thread (or the Pull Requests or Usage page), the composer and the panel; over them the picture viewer, the palette, and the link dialog when a link asks to connect |
| `views/pairing.rs` | The server on this computer when the core offers one (Connect, no code), then a pairing link, or server URL and code |
| `views/link_dialog.rs` | The question a link from outside asks before anything connects: a scrim over the window with a panel on it |
| `views/sidebar.rs` | Threads by status section (with project icons) or by project, with their badges and menus; the server, its companion and its switcher; Settings, New thread, and More (grouping, the palette, the Pull Requests and Usage pages); the search and the unread filter |
| `views/new_thread.rs` | Choosing a project: search, recent and pinned first, a grid of tiles, the Add menu, then local folder or worktree |
| `views/thread.rs` | The open thread's page: its header (usage, compact, fork, find, the panel, its menu), the find box, its rows (the last `transcript::WINDOW` entries, more on request), the working row, what waits in it, and the plan and checklist above the composer |
| `views/transcript.rs` | What the page keeps (what is opened, how much is shown, whether it follows the end, the picture viewer) and what its rows report (`Act`); rows as entries, steps of one turn together |
| `views/items.rs` | A row of the transcript by its kind: messages, reasoning, steps with their fields, output and diffs, groups, to-dos, plans, checkpoints, subagents, pictures |
| `views/markdown.rs` | Markdown blocks: rich text with its links, code in its colours, lists, quotes, tables, Mermaid diagrams the server drew, and a button for each workspace file a block links to |
| `views/panel/` | The panel beside the thread: `mod.rs` its tabs and what it keeps (`State`, `Act`), `diff.rs`, `files.rs` (the tree, a search, the file opened and its editing), `git.rs`, `terminal.rs` (the screen as lines of text, keys and typed characters back as events), `artifacts.rs` |
| `views/palette.rs` | The command palette: the core finds and runs, here the search is typed and a row is picked |
| `views/pull_requests.rs` | The Pull Requests page: the list, the one chosen, what can be done with it, a review and a comment |
| `views/usage.rs` | The Usage page: totals, a bar for each hour or day, the token kinds, each provider account's limits, and tables by model and by day |
| `views/pending.rs` | What waits in a thread: approvals (with the change an edit would make), the agent's questions and their answers, secrets it asks for, queued messages. `State` holds what is ticked and typed until it is sent |
| `views/composer.rs` | The draft (the window's own copy, see below) and its keys (`key_action`: Enter sends, the arrows go through a list or earlier messages); the lists `@`, `/` and `$` open; pictures; the model list and its options; effort, plan mode, permissions, limits; put aside, steer, stop, send |
| `views/settings/` | The Settings page: `mod.rs` the sections and their navigation, `general.rs` appearance, the connected server and its settings, the servers; `companion.rs` a server's companion; `server.rs` projects, access and pairing (QR), archived threads, maintenance; `providers.rs` providers, keybindings, scheduled tasks; `task.rs` a scheduled task's editor (`TaskDraft`) |
| `theme.rs` | Sizes, brushes and the icon font |

Shared with the Iced app, in `crates/domovoi-desktop-host`: `palette` (project colours and a terminal's), `prefs` (appearance, text size, where the window was), `system` (sleep and network changes) and `attach` (a picture as a message carries it). The Iced app still has its own copies of all four.

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
- A scroll viewer measures a centred, width-limited column wider than its
  card. Give such a page a `Grid` with a `max_width` and let a list inside
  it scroll (`views/new_thread.rs`).
- A control that shows a choice (`SelectorBar`, `ComboBox`, `ToggleSwitch`)
  reports the choice when it is set from the view model too. Tell the core
  only what differs from what it already has.
- A handler for a key decides at once whether the key is used up, before
  the window's state is at hand. What it needs to know is put in a `Cell`
  each time the window draws (`composer::Keys`).
- A text box set from the core has its caret at the start: after the core
  replaces the draft, the caret is sent to the end (`Message::CaretToEnd`).
- A text block gives no width to the space that ends its text. Pieces set
  side by side (highlighted code, a terminal's line) would close up: end
  such a piece with a word joiner (`widgets::keep_last_space`).
- A key goes to the window's shortcuts first, then down to what has the
  keyboard. What should keep its keys (the terminal) says so in a `Cell`
  the shortcut handler reads (`MainWindow::terminal_keys`).
- A `Border` takes the keyboard when it is a tab stop: named keys come to
  `on_preview_key_down`, typed characters to `on_character_received`, a
  character outside the basic plane as two halves.
- The whole window is drawn again on every change, so a view caps what it
  draws: the last entries of a thread, 300 coloured lines of code, 400
  lines of a file's diff.
- A button's tooltip has the button's name. A script that finds a control
  by name has to ask for the one it can press (`tools/snapshots.ps1`).
- This crate's log lines are named for its binary, `Domovoi::…`, not for
  the package: a log filter has to say `Domovoi`.
- `tools/parity.py` counts the events and view fields named in this crate's
  `src`. Build events and read fields here, not in a shared crate.
- `cargo` stages the Windows App Runtime beside the exe from `build.rs`
  (`windows_reactor_setup::as_self_contained`). A warm cache can skip copying
  it, so the packaging script clears what an earlier build staged beside the
  exe and touches `build.rs` first.

## Links from outside

A `domovoi://` link that Windows hands the app (its command line, or a later
launch through `links`) goes to the core as `Event::OpenLink`. Only what the
user puts in the pairing form is `ConnectRemote`. The core answers with
`ConnectionView.link_request`, and nothing connects until the user does:
`views/link_dialog.rs` draws the request over the window's rows, on any
screen, and Cancel and the accepting button send `AnswerLinkRequest`. It is
part of the view, not a `ContentDialog`: it has no message loop and the
window holds no state for it. A press on the scrim does nothing. The scrim
stops the pointer only: keyboard focus stays where it was, under it.

## Tests

`cargo test -p domovoi-windows` runs the unit tests (store splitting and row
patches, launch parsing, the image cache, the project picker's grouping and
search, the dialogs' edits and what they confirm, settings sections, the QR
code, sidebar and transcript helpers, shortcuts, the terminal's keys and
lines, the task editor, the companion, the palette, the pages' helpers, the link dialog's name
line). `just windows-smoke` starts the app
against a running server with a fresh pairing code, and fails unless it
connects. `just windows-snapshots` starts a server of its own, walks the
screens through UI Automation and saves a picture of each in
`target/ui-snapshots/windows` (the new-thread page, both groupings, the
palette, each tab of the panel, each Settings section); it fails when a
screen cannot be reached. Add
a screen to `tools/snapshots.ps1` when one is added to the app. The packaging script checks that the installed app opens its window.
