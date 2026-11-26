# Windows hardening plan

What is left after the first Windows pass (`docs/windows.md`): what this
machine's testing turned up, what was never tested, and what a release
depends on. It is ordered by risk. Each item says why it matters, what to
do, and how to tell it's done.

**State at the start of this plan:**

- **Tests on Windows 11:**
  - `just windows-check`, `just windows-test` (557 server, 24 Iced, 5 WinUI
    app, 2 host, 1 launcher) and `just parity` pass.
  - The server's integration suites pass except `provider_turns` and
    `rpc_calls`.
- **Installers:** both MSIs build, validate and pass their smoke tests. An
  install, upgrade and uninstall cycle is clean in the real registry.
- **The WinUI app** is at M1, plus send, stop, approvals and choosing a project.
- **Not run yet:** the Linux and macOS builds of these changes, and the CI
  workflows.

## Progress (2026-11-25)

| Item | State |
|---|---|
| 1. Unix builds, CI | Not done: needs a macOS machine and a pushed branch. |
| 2. Real CLIs | Not done here (a live Codex turn ran in the first pass). |
| 3. `provider_turns`, `rpc_calls` | Done. 89 pass and 3 are ignored (the 2 Antigravity sign-in-link tests, which are Unix only, and 1 ignored everywhere). `rpc_calls`: 10 pass, 1 ignored. Both are in `just windows-test-server`, so in CI. Product bugs found and fixed: programs named without an extension (ACP `commandPath`, `npx`) were not found by `PATHEXT`; CRLF in plans and in ranged file reads; Antigravity's `BROWSER` list separator; a subagent's result lost when its transcript finished before its reply reached the session DB (both platforms); `\\?\` paths given to `cmd.exe`, git (read as ssh) and the folder browser; `-l` given to Windows shells. |
| 4. Exit and children | The tree is taken right after spawn, a failed job assignment is a warning, and the runtime's shutdown is bounded. Not done: a run with the server inside another job. |
| 5. Paths | The core's path helpers (`tools::file_name`, `strip_root`, `same_path`, `parent_dir`, `trim_separators`) are used in artifacts, highlighting, the file tree's project match, the folder browser and project titles. The server's folder browser takes `\`. The server and launcher declare `longPathAware`. Not done: a `client_replay` test with a Windows snapshot, and the WinUI app's manifest (windows-reactor-setup makes it). |
| 6. Signing | Not done: needs a certificate. |
| 7. Pipe, secrets | Done. The links pipe grants only this user (a test reads its DACL back). Secret files are created with an owner-only descriptor (`util::create_private`, tested). `doctor` checks every secret file. Not run: the check as a second local user. |
| 8. Graceful stop | Done for Pi and OpenCode: Ctrl+Break to their own process group, then the tree after the grace (tested with a child that handles `SIGBREAK`). Local dev servers can't get one: they are on another console (`docs/windows.md`). |
| 9. Logs | Done. The launcher writes `%LOCALAPPDATA%\Domovoi\logs\launcher.log` (the server's stderr at `warn`, each start and exit, its own panics). Both apps log daily to `<data>\logs`, with panics in `panic.log` at once. |
| 10. Registry, Task Scheduler | Done. The host uses `RegSetKeyValueW` and `RegGetValueW` (tested). `service status` reads the task's XML and its numeric last result, not the translated status column (tested). |
| 11. Terminals | Partly. `cmd.exe` is tested by `rpc_calls`, and Windows shells no longer get `-l`. Not done: Windows PowerShell 5.1, Git Bash, WSL, resize. |
| 12. Installer edges | `packaging/windows/cycle.ps1` (`just windows-installer-cycle`) covers upgrade, downgrade, repair, a feature added later, the two products' conflict (now exclusive: the full MSI replaces the server-only one and keeps its features) and uninstall. The MSI now stops only programs running from its own folder. Data left on uninstall and the firewall prompt are documented. Not done: running it in CI on a clean VM. |
| 13–16 | Not started. |
| 17 | The two `client_replay` failures are not about the clock: `limit_windows` orders windows by kind (session, weekly, monthly), and the tests expect the fullest first. Both came in `adf6350`; which one is meant needs a decision. Not done: registry policy, arm64, winget, Antigravity's browser. |

## P0: before anyone relies on it

### 1. Run the Unix builds and the CI workflows

- **Why.** Every server change has `cfg(unix)` branches that were never
  compiled here: `process.rs`, `shell_env.rs`, `local_servers.rs`,
  `service.rs`, `util.rs`, `keep_awake.rs`, `terminal.rs`. So were the moved
  Iced code (`links.rs`'s macOS Apple Event handler) and the host crate's Unix
  socket path. `just check` on macOS fails on any warning, including dead code
  that only shows up there.
- **Do:**
  - Run `just check` and `cargo test --workspace` on macOS.
  - Run `just check-linux` (Docker) and `just check-windows` (zig).
  - Push a branch so `ci.yml`'s new `windows` job runs, and run
    `windows-installers.yml` by hand.
- **Done when:** all three platforms are green in CI, and the installer
  workflow uploads both MSIs.

### 2. Real agent CLIs on Windows, end to end

- **Why.** The scripted fake found that std refuses to pass the Claude
  adapter's arguments to a `.cmd` file. `process::command` now starts npm and
  one-line shims as their interpreter and script. Real `claude.cmd`,
  `codex.cmd`, `opencode` and `pi` installs have not been run through it.
- **Do:**
  - Run `just live claude` and `just live codex` against npm-installed CLIs,
    and once more against native `.exe` installs (Claude's `~\.local\bin`,
    winget).
  - Send prompts holding `"`, `%`, `^`, `&`, newlines and non-ASCII text.
  - Start Antigravity (`agy.exe`) and an ACP registry agent.
  - When a `.cmd` that isn't a shim gets an argument it can't take, report a
    clear error that names the CLI, not std's "batch file arguments are
    invalid". The error could suggest the native installer.
- **Done when:** a turn, an approval, interrupt and resume work for each CLI.
  Task Manager shows no orphaned `node` after interrupting, and none after
  killing the server.

### 3. Make `provider_turns` and `rpc_calls` pass on Windows

- **Why.**
  - These two suites cover the agent adapters and the RPC surface: 36 of 91
    and 6 of 11 tests failed before the shim fix.
  - The fake CLIs are `@python "%~dp0name" %*` wrappers, which the shim fix
    now starts directly. That changes which tests get how far: the re-run
    after the fix did not finish within 25 minutes, where it took 46 s
    before. At least one test now waits forever. That run also left an
    orphaned fake `opencode serve` (item 4), which kept the test's output
    pipe open.
  - The rest assume Unix paths (`/usr/bin/false`, `tar`, POSIX `sh` in
    fixtures).
- **Do:**
  - Give every wait in the suite a deadline, so a stuck test fails with what
    it waited for instead of hanging the run.
  - Re-run with `--no-fail-fast` and sort the failures. Fix the product bugs.
    Port each fixture to a cross-platform form (temp paths, a Rust or Python
    fake instead of `sh`).
  - Gate a test with a written reason only when it is about Unix itself.
  - Add both suites to `just windows-test-server` and the CI job.
- **Done when:** both suites pass in the `windows` CI job.

### 4. The server exits when told to, and takes its children with it

- **Why.** Two things this session's test runs left behind:
  - **Orphans.** Nine fake `opencode serve` processes from the first
    `provider_turns` run were still alive hours after their servers had
    exited. They were started through `cmd.exe` (before the shim fix). Each
    agent child is put in a job that kills its tree when closed
    (`process::Tree`), and that didn't catch them. A likely reason: the
    server itself ran inside another job (the Claude desktop app's sandbox),
    and assigning to a nested job can fail. `Tree::of` only logs that at
    debug level.
  - **Hung shutdown.** `domovoi-server stop` with a turn still open: the
    server stopped listening but its process never exited, and its agent
    (the fake Claude) stayed alive with it.
- **Do:**
  - Log a failed job assignment as a warning, once.
  - Find out when nesting fails and fall back: one job for the whole server
    with `JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK` (as the launcher does), plus
    `KILL_ON_JOB_CLOSE` on each child's own job when it can be made.
  - Find what shutdown waits on with a live turn, and bound it: end the
    agents' trees after the grace period (item 8), then exit.
  - Add an integration test: start a turn, run `domovoi-server stop`, and
    check the server and every process it started are gone within a few
    seconds. Run it once more with the server inside a job.
- **Done when:** that test passes in CI and in a terminal inside the Claude
  desktop app, and Task Manager shows no `node` or `python` left after a
  `provider_turns` run.

### 5. A sweep for Windows paths in the client core and the server

- **Why.** The screenshots showed a project titled with its whole Windows
  path: the core split server paths on `/` only, fixed in `tools::file_name`.
  The same pattern is left in 9 places in `apps/domovoi-core` (`artifacts.rs`,
  `highlight.rs`, `present.rs`, …) and 13 in `apps/server`. The core runs on
  the client, which can be on any platform while the server is on Windows.
- **Do:**
  - Every place that takes a name, a parent or a relative path from a path
    the server sent uses one helper that knows both separators.
  - Compare workspace roots and paths case-insensitively and with
    `\\?\`-normalization when the server is on Windows (the core knows the
    server's `platform`).
  - Check `file://` links, the file tree and `@` mentions with `C:\…` roots.
  - Check paths over 260 characters: a `longPathAware` manifest for the
    server and the app, with `std::fs` getting `\\?\` prefixes where it
    needs them.
- **Done when:** a test in `client_replay` drives a Windows server's
  snapshot (drive letters, backslashes, mixed case) through the sidebar, the
  thread header, file links and the file tree.

### 6. Signing, before handing out an MSI

- **Why.** The programs and MSIs are unsigned, so SmartScreen warns and
  Defender may quarantine `domovoi-server-launcher.exe`. The launcher starts
  hidden processes, which is a common malware heuristic.
- **Do:**
  - Get a code-signing certificate (OV, or EV for immediate SmartScreen
    reputation) and add `WINDOWS_CERT_PFX` and `WINDOWS_CERT_PASSWORD` to the
    repo's secrets.
  - `package.ps1` already signs when `DOMOVOI_SIGN_CERT_THUMBPRINT` is set.
  - Add a check that every shipped `.exe` and the `.msi` carries a valid
    signature (`Get-AuthenticodeSignature`).
- **Done when:** the CI installers are signed and timestamped, and a fresh
  Windows VM installs one without a SmartScreen block.

## P1: robustness and security

### 7. The named pipe and other local channels

- **Why.**
  - The single-instance pipe (`links.rs`) rejects remote clients but keeps
    the default DACL. On a shared machine another local user could open the
    pipe and hand the app a `domovoi://` link. The Unix socket is mode 0600.
  - The secret files get their ACL after they are written, so for a moment
    they carry the folder's inherited one.
- **Do:**
  - Create the pipe with a security descriptor that grants only the current
    user.
  - Create secret files with that descriptor from the start
    (`CreateFileW` with `SECURITY_ATTRIBUTES`) instead of setting it
    afterwards (`win::restrict_to_current_user`).
  - Make `doctor` check every secret file, not only the iroh key.
- **Done when:** a test as a second local user can't open the pipe or read a
  secret file.

**Since then (not run on Windows).** The server also closes `userdata`,
`remote` and `local` under its home to other accounts at each start
(`Paths::make_private`, `util::tighten`), writes `remote.json` with an ACL of
its own, and signs apps on its machine in without a code (`local.rs`). All of
it was written and tested on macOS; on Windows it has only been cross-compiled.
To check on a Windows machine:

- `just windows-test-server` (the `remote_access` and `pairing_permissions`
  suites carry the new cases; the file-mode ones are Unix only).
- After a start, `icacls %USERPROFILE%\.domovoi\userdata` lists your account
  alone, and the home folder itself is unchanged.
- The first start on a home with a large `userdata`: setting a protected ACL
  on the folder rewrites the inherited entries of everything under it, once.
  Time it on a real home.
- A fresh WinUI or Iced app beside a running server connects with no code;
  the same app run elevated shows the pairing form (its request file is
  owned by Administrators, which `local::read_request` refuses).
- As a second local user: `local\server.json` and `local\requests` cannot be
  read or written.

### 8. Stopping agents gracefully

- **Why.** Windows has no SIGTERM. Stopping a turn or a local dev server ends
  the process tree at once (`TerminateJobObject`, `TerminateProcess`), so an
  agent can't save its session and a dev server can't clean up.
- **Do:**
  - Start agent CLIs with `CREATE_NEW_PROCESS_GROUP` and send
    `CTRL_BREAK_EVENT` first (`GenerateConsoleCtrlEvent`).
  - Wait for the grace period the Unix paths use (`pi_rpc`'s 1 s, OpenCode's
    3 s), then end the job.
  - Do the same for `preview.stopLocalServer`.
- **Done when:** after Stop, Claude resumes its session on the next turn,
  and a stopped `vite` has released its port.

### 9. Logs and crash reports from the Windows programs

- **Why.**
  - The WinUI app is a windowed program and logs to stderr, which nobody
    sees: its failures leave no trace.
  - The launcher discards the server's stdout and stderr. The server keeps
    its own log files, but a panic before logging starts is lost.
- **Do:**
  - The app writes a rolling log in its data folder (`domovoi-windows\logs`),
    as the server does.
  - The launcher appends the server's stderr to
    `userdata\logs\launcher.log` and logs each restart with its exit code.
  - Install a panic hook in both programs that writes the panic to the log.
- **Done when:** a forced panic in each program leaves a readable report, and
  `docs/windows.md` says where to find it.

### 10. The host's registry and command use

- **Why.** The host (`crates/domovoi-desktop-host/src/windows.rs`) registers
  the scheme and app id by running `reg.exe` nine times. That is slow at
  startup, and it decides by searching `reg`'s output, which depends on the
  locale. `service.rs` likewise reads `schtasks`'s status column, which this
  Polish system printed in Polish.
- **Do:**
  - Use the Registry API (`RegSetKeyValueW`, `RegGetValueW`) in the host.
  - Use the Task Scheduler COM API, or `schtasks /XML` plus parsing the XML
    for state, in `service.rs`.
- **Done when:** both work on a non-English Windows, which the Windows CI
  runner can simulate with `Set-WinSystemLocale`.

### 11. Terminals on Windows

- **Why.** The server now answers ConPTY's first cursor query, and that was
  tested only with `pwsh`. Not tested: `cmd.exe`, Windows PowerShell 5.1, Git
  Bash, `wsl.exe`, full-screen programs (`vim`, `htop` in WSL) and resizing.
- **Do:**
  - Add a test per shell in `terminal.rs`, behind a check that the shell
    exists.
  - Check later `ESC[6n` queries reach the client's emulator and are answered
    there, or answer them in the server from the client's last known cursor.
- **Done when:** each shell gets a prompt, runs a command and resizes in the
  Iced app's terminal panel, and in the WinUI app's once it has one.

### 12. Installer edges

- **Do:**
  - **Upgrade.** Build `0.1.1` over an installed `0.1.0`. The user's
    settings, pairing and the "Start at sign-in" choice must survive.
  - **Downgrade.** Installing an older version must be refused with the
    message.
  - **Repair.** Run `msiexec /fa`.
  - **Features.** Install the full MSI with only the server, then add the
    Desktop feature later from Apps & features.
  - **Conflict.** Install the server-only MSI beside the full one.
    `domovoi-server` must not end up twice on PATH. Either make the two
    products exclusive or share the server component.
  - **Data.** Uninstall leaves `%APPDATA%\domovoi-windows` and
    `~\.domovoi`. Document that, or ask in the uninstall UI.
  - **Firewall.** A server started with `--host 0.0.0.0` triggers the
    firewall prompt. Document it. A per-user install can't add a rule.
  - **Test outside the Claude desktop app.** Its child processes see a
    private `HKCU\Software\Classes`; this session's checks used a scheduled
    task.
- **Done when:** a scripted cycle, `packaging/windows/cycle.ps1` run in CI,
  passes for each case on a clean VM.

## P2: the WinUI app to parity, and long-term upkeep

### 13. Milestones M2 to M4

- **Missing, by milestone:**
  - **M2:** question answers (`RespondUserInput`), attachments, the model and
    effort picker, the runtime mode.
  - **M3:** the terminal panel, settings, the command palette.
  - **M4:** pull requests, usage, agent sessions, the folder browser,
    diagrams and images (`HostEvent::ImageLoaded` is ignored today).
- **How:** take each from the Iced app (`apps/domovoi-iced/src/views`) and
  the macOS views as the spec.
- **Done when:** `windows` leaves `WIP_CLIENTS` in `tools/parity.py`, and
  `just parity` enforces it like the other clients.

### 14. Transcripts, accessibility and display

- **Why.**
  - The transcript is one `StackPanel` with every item: a long thread builds
    thousands of elements, and every delta rebuilds them.
  - The screenshots also showed layout was only tried at one window size.
- **Do:**
  - Draw the transcript with a virtualized `ItemsRepeater` (lwa_fm's
    `files\` module), keyed by item id.
  - Test at 125 %, 150 % and 200 % scaling, at the minimum window size, in
    light mode, in high contrast, and with Narrator.
  - Check keyboard navigation: Tab order, Ctrl+Enter, Escape.
- **Done when:** a 5,000-item thread scrolls smoothly, and the UIA tree
  names every control.

### 15. UI tests in CI

- **Why.** The screenshots were taken with a UI Automation and `PrintWindow`
  script, written for this session, that drives a real server with the
  scripted fake Claude. It can become a regression test.
- **Do:**
  - Move it into `apps/domovoi-windows/tools/` as `just windows-snapshots`.
    It pairs, adds a project, sends a message, then checks the UIA tree (the
    project title, the transcript items, an approval card) and saves the
    images.
  - Run it in the `windows` CI job (GitHub's Windows runners have a desktop
    session) and upload the images as an artifact.
- **Done when:** a broken layout or a missing view fails CI.

### 16. The windows-rs fork

- **Why.** The app pins Leinnan's fork (`dirfleet` branch, one rev) for
  scroll-to, TextBox selection and wheel and X-button input. A fork drifts
  from upstream's fixes and Windows App SDK updates.
- **Do:**
  - Track the fork's rev together with lwa_fm.
  - Upstream the additions to microsoft/windows-rs, or reduce the app to
    upstream's API where it can live without them.
  - Note the Windows App SDK version that `windows-reactor-setup` stages, and
    test each bump.
- **Done when:** the app builds on upstream windows-rs, or the fork's
  difference is written down and re-checked at each bump.

### 17. Leftovers from the first pass

- **Antigravity sign-in.** The link opens a browser on the server's machine.
  Find out how `agy.exe` opens it (ShellExecute, a `BROWSER` variable) and
  intercept it if there's a way.
- **Machine policy from the registry.** Read `HKLM\Software\Policies\Domovoi`
  as well as `policy.json`.
- **arm64.** Build `aarch64-pc-windows-msvc` and test `windows-reactor-setup`
  there, then add an arm64 MSI.
- **Updates.** Domovoi has no self-update. Publish a winget manifest from
  the release workflow, then consider an in-app update check.
- **Two core tests that disagree with the code.** `client_replay`'s
  `a_thread_shows_its_context_limits_tasks_and_plans_in_detail` and
  `the_header_meter_shows_the_thread_providers_limits` fail on every
  platform. They expect the fullest usage window first; `limit_windows`
  orders them by kind. (An earlier note here blamed the clock: the tests fix
  `now_ms`.) Decide which order is meant, and make the other match.

## Order

1. **First:** items 1–4. They tell whether what exists works, and stop it
   leaving processes behind.
2. **Next:** 5 and 6, before handing out an installer.
3. **Then:** 7–12, in any order (they touch separate files).
4. **Alongside:** 13–15, as the app grows.
5. **When needed:** 16 and 17.
