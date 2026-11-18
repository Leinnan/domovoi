#!/usr/bin/env bash
# Cross-builds domovoi-server for Windows (x86_64-pc-windows-gnu) with zig as the
# C toolchain: `cargo install cargo-zigbuild`, `rustup target add
# x86_64-pc-windows-gnu`, and zig on PATH. It checks that the Windows code
# compiles and links; it cannot run it.
set -euo pipefail
cd "$(dirname "$0")/../.."
work=target/cross-windows
mkdir -p "$work/bin" "$work/lib"
# rustc looks for a mingw dlltool; zig ships one under another name.
printf '#!/bin/sh\nexec zig dlltool "$@"\n' > "$work/bin/x86_64-w64-mingw32-dlltool"
chmod +x "$work/bin/x86_64-w64-mingw32-dlltool"
# zig's mingw has no import library for the WaitOnAddress family.
printf 'LIBRARY api-ms-win-core-synch-l1-2-0.dll\nEXPORTS\nWaitOnAddress\nWakeByAddressAll\nWakeByAddressSingle\n' > "$work/lib/synchronization.def"
zig dlltool -d "$work/lib/synchronization.def" -l "$work/lib/libsynchronization.a" -m i386:x86-64
PATH="$PWD/$work/bin:$PATH" CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-L $PWD/$work/lib" \
  cargo zigbuild --target x86_64-pc-windows-gnu -p domovoi-server "$@"
