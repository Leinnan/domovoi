# Apple thread list project icons

The shared core resolves automatic identities, saved monograms, emoji, Lucide names, and legacy Lucide monograms. Thread rows carry the resolved project icon, including rows grouped by status. Automatic identities follow the web implementation, including NFKC normalization and its palette hash. In particular, `api-v2` produces `AV`, while `api2` produces `A2`: the web only searches for a digit within the first word.

On macOS, use a project header’s **Change Icon…** context menu. On iOS, tap the project header’s icon (or use its context menu). The picker offers Automatic, Letters, Emoji, and Symbol. Saves update immediately; failed requests restore the previous icon unless another choice has superseded it. The server stores the same icon record used by the web app.

Rows use a separate unread gutter, two text lines on macOS, project avatars with status badges in status groups, and status glyphs beneath project headers. iPhone uses a plain list and iPad uses a sidebar. Accessibility text sizes stack metadata beneath the title; avatars stop growing at 44 pt. Decorative icons are hidden from accessibility, rows expose combined labels, and working indicators respect Reduce Motion.

## Verification (2026-11-19)

- `cargo test -q -p domovoi_core`: passed, including identity, icon resolution, request encoding, optimistic updates, and rejection rollback tests.
- `just mac-generate`, `just android-typegen`: passed.
- Xcode Debug builds of `Domovoi` and `DomovoiIOS`: passed. Existing warnings remain in unrelated iOS code.
- Recorded-session view walk: generated 969 digests successfully. The external comparison baselines required by the ignored digest tests were not supplied, so no baseline recordings were re-blessed.
- Reviewed macOS captures in status/project groupings and light/dark appearance; iPhone default/AX5; iPad split sidebar.
- Saved a green terminal icon from the iPhone picker, checked its server record, and confirmed it in macOS and iPad captures using separate in-memory client stores.
- Palette contrast script: minimum 4.72:1 on a light sidebar surface and 4.93:1 on a dark surface, across all 18 monogram colors.
- Inspected combined row and section accessibility labels. Spoken VoiceOver output and the web UI were not manually exercised.

Captures are local, ignored artifacts in `.reference/thread-list-icons/`.

## Focused capture modes

Use a disposable server/database and `DOMOVOI_STORE=memory` for these runs. macOS accepts `DOMOVOI_SNAPSHOT_DIR`, `DOMOVOI_SERVER`, `DOMOVOI_PAIRING`, and `DOMOVOI_SIDEBAR_SNAPSHOTS=1` to capture both groupings and appearances and then quit.

For iOS simulator launches, pass the corresponding variables with the `SIMCTL_CHILD_` prefix: `DOMOVOI_WALK=1`, `DOMOVOI_WALK_SIDEBAR_ONLY=1`, `DOMOVOI_SERVER`, and `DOMOVOI_PAIRING`. The walker prints `walk: threads` after connecting and expanding the sections, then stays on the list. `DOMOVOI_WALK_GROUPING=project` selects project grouping instead.
