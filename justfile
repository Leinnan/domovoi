# domovoi: the Rust server, the Crux client core and the macOS and iPhone apps.
# `just` lists the recipes. Packaging needs Xcode, xcodegen and boltffi
# (`cargo install boltffi_cli`); the Mac builds are Apple silicon only
# (Rust target aarch64-apple-darwin).

set shell := ["bash", "-euo", "pipefail", "-c"]

version := `sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1`
mac := "apps/domovoi-apple"
derived := mac / "build"
dist := "dist"
home := ".reference/domovoi-home"
port := "13774"
sim := "iPhone 17 Pro"
ios_bundle := "com.crayen.domovoi"
droid := "apps/domovoi-android"
android_sdk := env("ANDROID_HOME", env("HOME") / "Library/Android/sdk")
android_ndk := android_sdk / "ndk/29.0.13846066"
android_jdk := env("JAVA_HOME", "/Applications/Android Studio.app/Contents/jbr/Contents/Home")
adb := android_sdk / "platform-tools/adb"
avd := "Pixel_5"
# The Linux machine `just linux-*` deploys to and manages (ssh destination).
linux_host := env("DOMOVOI_LINUX_HOST", "piotr@192.168.1.99")
android_id := "com.crayen.domovoi"
# Size budgets (MB) of the Mac apps' executables; `mac-package` fails above them.
client_budget := "30"
server_budget := "60"
lsregister := "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

# Signing team for the macOS release and iPhone apps.
export DEVELOPMENT_TEAM := env("DEVELOPMENT_TEAM", "LALG9U2L97")

default:
    @just --list --unsorted

# --- Rust ---

# Build everything (debug)
build:
    cargo build --workspace

# Check out the t3code release in conformance/t3code-pin under .reference/t3code and
# install its packages: the schemas the conformance checks decode with (needs pnpm)
reference:
    #!/usr/bin/env bash
    set -euo pipefail
    read -r tag commit < <(grep -v '^#' conformance/t3code-pin)
    if [ ! -d .reference/t3code/.git ]; then
      git clone -q --filter=blob:none https://github.com/pingdotgg/t3code.git .reference/t3code
    fi
    git -C .reference/t3code fetch -q --tags origin
    git -C .reference/t3code checkout -q "$commit"
    [ "$(git -C .reference/t3code rev-parse "$tag^{commit}")" = "$commit" ] || { echo "reference: $tag is not $commit" >&2; exit 1; }
    (cd .reference/t3code && pnpm install --frozen-lockfile)

# Every test; the schema checks also run when Node 26+ is on PATH (or in DOMOVOI_NODE)
test:
    DOMOVOI_NODE="${DOMOVOI_NODE:-$(command -v node || true)}" cargo test --workspace

# Every RPC method called for each answer and failure it can give, group by
# group (conformance/rpc-groups); six to ten minutes. `just rpc-groups vcs-status,vcs-refs` runs some
rpc-groups groups="":
    DOMOVOI_NODE="${DOMOVOI_NODE:-$(command -v node || true)}" {{ if groups == "" { "" } else { "DOMOVOI_GROUPS=" + groups } }} cargo test -p domovoi-server --test rpc_calls every_rpc_group_answers_as_recorded -- --ignored --nocapture

# Turns of the real agent CLIs (spends tokens): `just live claude`, `just live codex`,
# `just live claude native_fork`, `just live antigravity`. Models: DOMOVOI_LIVE_CLAUDE_MODEL (default
# claude-haiku-5-5), DOMOVOI_LIVE_CODEX_MODEL (default gpt-6-luna). Every frame is
# recorded and decoded with t3code's schemas afterwards (Node 26+ in DOMOVOI_NODE).
live provider filter="":
    #!/usr/bin/env bash
    set -uo pipefail
    case "{{provider}}" in
      claude) export DOMOVOI_LIVE_CLAUDE_MODEL="${DOMOVOI_LIVE_CLAUDE_MODEL:-claude-haiku-5-5}" ;;
      codex) export DOMOVOI_LIVE_CODEX_MODEL="${DOMOVOI_LIVE_CODEX_MODEL:-gpt-6-luna}" ;;
      # Both: what goes from one to the other (a provider switch).
      both) export DOMOVOI_LIVE_CLAUDE_MODEL="${DOMOVOI_LIVE_CLAUDE_MODEL:-claude-haiku-5-5}" DOMOVOI_LIVE_CODEX_MODEL="${DOMOVOI_LIVE_CODEX_MODEL:-gpt-6-luna}" ;;
      # Signs a fresh profile in (finishes by itself only when Google asks for no browser).
      antigravity) export DOMOVOI_LIVE_ANTIGRAVITY_MODEL="${DOMOVOI_LIVE_ANTIGRAVITY_MODEL:-gemini-3.8-flash}" ;;
      *) echo "live: claude, codex, both or antigravity" >&2; exit 2 ;;
    esac
    out="${DOMOVOI_WIRE_OUT:-target/live-{{provider}}.ndjson}"
    rm -f "$out"
    DOMOVOI_WIRE_OUT="$PWD/$out" cargo test -p domovoi-server --test provider_turns "live::{{ if provider == "both" { "a_thread_switched" } else if provider == "antigravity" { "antigravity::" + filter } else { provider + "::" + filter } }}" -- --test-threads="${LIVE_THREADS:-{{ if provider == "antigravity" { "1" } else { "3" } }}}" --nocapture
    status=$?
    node="${DOMOVOI_NODE:-$(command -v node || true)}"
    if [ -n "$node" ] && [ -s "$out" ]; then
      "$node" conformance/wire-check.ts "$out" || status=1
    fi
    exit $status

# Every client uses every core event and view field, bar tools/parity-allowlist.toml (`--matrix` prints counts)
parity *args:
    python3 tools/parity.py {{args}}

# Build everything and fail on any compiler warning or lint-gate finding
check: lfs-check parity
    #!/usr/bin/env bash
    set -uo pipefail
    for cmd in "cargo build --workspace" "cargo test --workspace --no-run"; do
      out=$($cmd 2>&1) || { echo "$out"; exit 1; }
      # A dependency's future-incompatibility notice (block 0.1.6, pulled in by
      # iced on macOS) is not a warning about this code.
      if grep -v 'will be rejected by a future version of Rust' <<<"$out" | grep -E -A6 '^(warning|error)'; then exit 1; fi
    done
    # The lint gate, outside tests: no `unwrap()` (an `expect` names its
    # invariant), no std lock held across an await, no oversized enum variant,
    # and nothing `clippy.toml` disallows (a child process started outside
    # `process.rs`, a queue with no limit that does not say what bounds it,
    # JSON held as JSON outside a module that says whose format it reads).
    cargo clippy -q --workspace --lib --bins -- -A clippy::all \
      -D clippy::unwrap_used -D clippy::await_holding_lock -D clippy::large_enum_variant \
      -D clippy::disallowed_methods -D clippy::disallowed_types -D clippy::disallowed_macros \
      -D unfulfilled_lint_expectations

# The checkout has its Git LFS files (images and icons), not pointer files
# in their place, and every file a `.gitattributes` rule puts in LFS is stored
# there. `check` and the packaging recipes run it first.
lfs-check:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
      echo "lfs-check: not a git checkout; skipped"; exit 0
    fi
    if ! git lfs version >/dev/null 2>&1; then
      echo "lfs-check: Git LFS is not installed (https://git-lfs.com). Install it, then run: git lfs install && git lfs pull" >&2
      exit 1
    fi
    # `git lfs ls-files` marks a file whose working copy is still a pointer with `-`.
    pointers=$(git lfs ls-files | awk '$2 == "-" { $1 = ""; $2 = ""; sub(/^  /, ""); print }')
    if [ -n "$pointers" ]; then
      echo "lfs-check: these Git LFS files are pointer files, not their contents:" >&2
      sed 's/^/  /' <<<"$pointers" >&2
      echo "Run: git lfs pull" >&2
      exit 1
    fi
    # Files a rule puts in LFS but committed as plain blobs, and the reverse.
    git lfs fsck --pointers >/dev/null || { git lfs fsck --pointers >&2; exit 1; }
    echo "lfs-check: $(git lfs ls-files | wc -l | tr -d ' ') Git LFS files present"

# Run the server (debug) on `port` with state in `home`
server port=port home=home:
    cargo run -p domovoi-server -- --home-dir {{home}} --port {{port}}

# Print a pairing link for a running server
pair port=port home=home:
    cargo run -q -p domovoi-server -- --home-dir {{home}} --port {{port}} pair

# Run the server with its status bar icon
tray port=port home=home:
    cargo run -p domovoi-server -- --home-dir {{home}} --port {{port}} --tray

# Remote access of a running server: `just remote status|on|off|devices`, `just remote revoke <id>`
remote *args:
    cargo run -q -p domovoi-server -- --home-dir {{home}} --port {{port}} remote {{args}}

# Cross-build the server for Windows (compiles and links; cannot run it). Needs cargo-zigbuild and zig
check-windows:
    tools/cross/windows.sh

# Build and test the server on Linux in Docker (the repo is mounted read-only)
check-linux *args="test --no-fail-fast -p domovoi-server -p domovoi-remote -p domovoi-model -p effect-rpc":
    docker build -q -t domovoi-linux -f tools/cross/linux.Dockerfile tools/cross >/dev/null
    # No debug info: the build cache stays a few GB instead of filling Docker's disk.
    docker run --rm -e CARGO_PROFILE_DEV_DEBUG=0 -e CARGO_PROFILE_TEST_DEBUG=0 -v "$PWD":/src:ro -v domovoi-linux-target:/target -v domovoi-linux-cargo:/usr/local/cargo/registry domovoi-linux cargo {{args}}

# --- Apple apps (macOS and iOS) ---

# Swift types for the core's events and view model
mac-typegen:
    cargo run -q -p domovoi_core --bin codegen --features codegen -- --output-dir {{mac}}/generated

# The core as a Swift package (xcframework: macOS arm64, iOS, simulator); `release=true` optimizes it
mac-ffi release="false":
    cd apps/domovoi-core && boltffi pack apple {{ if release == "true" { "--release" } else { "" } }}

# Generated sources and the Xcode project
mac-generate release="false": mac-typegen (mac-ffi release)
    cd {{mac}} && xcodegen -q

# Package only the macOS slice for the Mac release.
[private]
_mac-release-generate: mac-typegen
    cd apps/domovoi-core && boltffi pack apple --release --overlay boltffi-macos.toml
    cd {{mac}} && xcodegen -q --spec project-macos.yml

# Debug build of the app ("Domovoi Dev", com.crayen.domovoi.mac.debug), the smoke tool and "Domovoi Server.app" beside the app
mac-build: mac-generate
    for scheme in Domovoi DomovoiSmoke; do \
      xcodebuild -project {{mac}}/Domovoi.xcodeproj -scheme $scheme -configuration Debug -derivedDataPath {{derived}} \
        ONLY_ACTIVE_ARCH=YES CURRENT_PROJECT_VERSION=$(git rev-list --count HEAD) -skipPackagePluginValidation -quiet build; \
    done
    @"{{just_executable()}}" --justfile "{{justfile()}}" tray-app "{{derived}}/Build/Products/Debug" false
    @"{{just_executable()}}" --justfile "{{justfile()}}" _mac-register "{{derived}}/Build/Products/Debug/Domovoi.app"
    @echo "{{derived}}/Build/Products/Debug/Domovoi.app"

# Build and launch the debug app
mac-run: mac-build
    open {{derived}}/Build/Products/Debug/Domovoi.app

# Headless check of the app's core against a running server (mints a pairing
# code). Fails if the main thread stalls longer than half a frame.
mac-smoke url=("http://127.0.0.1:" + port) home=home:
    code=$(cargo run -q -p domovoi-server -- --home-dir {{home}} --port {{port}} pair | grep -o 'token=[A-Z0-9-]*' | cut -d= -f2); \
    DOMOVOI_STORE=memory DOMOVOI_SMOKE_MAX_STALL_MS="${DOMOVOI_SMOKE_MAX_STALL_MS:-8}" {{derived}}/Build/Products/Debug/DomovoiSmoke {{url}} "$code"

# The same check over iroh (needs the server started with --remote)
mac-smoke-remote home=home:
    link=$(cargo run -q -p domovoi-server -- --home-dir {{home}} --port {{port}} pair --remote | sed -n 's/^link: //p'); \
    DOMOVOI_STORE=memory {{derived}}/Build/Products/Debug/DomovoiSmoke "$link"

# Walk the app's screens against a running server and save window captures
mac-snapshots out=".reference/snapshots" url=("http://127.0.0.1:" + port) home=home:
    rm -rf {{out}}
    code=$(cargo run -q -p domovoi-server -- --home-dir {{home}} --port {{port}} pair | grep -o 'token=[A-Z0-9-]*' | cut -d= -f2); \
    DOMOVOI_STORE=memory DOMOVOI_SNAPSHOT_DIR="$PWD/{{out}}" DOMOVOI_PAIRING="$code" DOMOVOI_SERVER={{url}} \
      {{derived}}/Build/Products/Debug/Domovoi.app/Contents/MacOS/Domovoi >/dev/null 2>&1 || true
    @ls {{out}}

# iPhone app (debug) for the simulator `sim`
ios-build sim=sim: mac-generate
    xcodebuild -project {{mac}}/Domovoi.xcodeproj -scheme DomovoiIOS -configuration Debug -derivedDataPath {{derived}} \
      -destination 'platform=iOS Simulator,name={{sim}}' -skipPackagePluginValidation -quiet build
    @echo "{{derived}}/Build/Products/Debug-iphonesimulator/DomovoiIOS.app"

# Build, install and launch the iPhone app in the simulator
ios-run sim=sim: (ios-build sim)
    xcrun simctl boot "{{sim}}" 2>/dev/null || true
    open -a Simulator
    xcrun simctl install "{{sim}}" {{derived}}/Build/Products/Debug-iphonesimulator/DomovoiIOS.app
    xcrun simctl launch "{{sim}}" {{ios_bundle}}

# Pair the simulator with a running server over iroh (opens a fresh domovoi:// link)
ios-pair sim=sim home=home:
    link=$(cargo run -q -p domovoi-server -- --home-dir {{home}} --port {{port}} pair --remote | sed -n 's/^link: //p'); \
    xcrun simctl openurl "{{sim}}" "$link"

# iPhone app (debug) for a physical device, signed with DEVELOPMENT_TEAM
ios-device: mac-generate
    xcodebuild -project {{mac}}/Domovoi.xcodeproj -scheme DomovoiIOS -configuration Debug -derivedDataPath {{derived}} \
      -destination 'generic/platform=iOS' -allowProvisioningUpdates -skipPackagePluginValidation -quiet build
    @echo "{{derived}}/Build/Products/Debug-iphoneos/DomovoiIOS.app"

# Build, install and launch on a connected iPhone (`xcrun devicectl list devices` names it)
ios-device-run device: ios-device
    xcrun devicectl device install app --device "{{device}}" {{derived}}/Build/Products/Debug-iphoneos/DomovoiIOS.app
    xcrun devicectl device process launch --device "{{device}}" {{ios_bundle}}

# Open the generated Xcode project
ios-open: mac-generate
    open {{mac}}/Domovoi.xcodeproj

# Device archive of the iPhone app, signed with DEVELOPMENT_TEAM
ios-archive: (mac-generate "true")
    xcodebuild -project {{mac}}/Domovoi.xcodeproj -scheme DomovoiIOS -configuration Release -destination 'generic/platform=iOS' \
      -archivePath {{dist}}/DomovoiIOS.xcarchive -allowProvisioningUpdates MARKETING_VERSION={{version}} \
      CURRENT_PROJECT_VERSION=$(git rev-list --count HEAD) -skipPackagePluginValidation -quiet archive
    @echo "{{dist}}/DomovoiIOS.xcarchive"

# Archive the iPhone app and upload it to TestFlight; `marketing` overrides the Cargo.toml version, BUILD=n the build number (docs/testflight.md)
testflight marketing="":
    JUST="{{just_executable()}}" DOMOVOI_VERSION={{version}} tools/ios/testflight.sh {{marketing}}

# Same archive and .ipa export as testflight, validated with App Store Connect but not uploaded
testflight-validate marketing="":
    JUST="{{just_executable()}}" DOMOVOI_VERSION={{version}} tools/ios/testflight.sh --validate-only {{marketing}}

# Print the Developer ID identity for the configured team, or fail before building.
[private]
_mac-client-identity:
    #!/usr/bin/env bash
    set -euo pipefail
    identities=$(security find-identity -v -p codesigning)
    identity=$(awk -v team="($DEVELOPMENT_TEAM)" \
      '/"Developer ID Application:/ && index($0, team "\"") && !found++ { print $2 }' <<<"$identities")
    if [[ -z "$identity" ]]; then
      echo "mac-release requires a Developer ID Application signing identity for team $DEVELOPMENT_TEAM" >&2
      exit 1
    fi
    echo "$identity"

# Release build (arm64, stripped) of the apps, with a stable Keychain identity for the client
mac-release: _mac-client-identity _mac-release-generate
    xcodebuild -project {{mac}}/DomovoiMacRelease.xcodeproj -scheme Domovoi -configuration Release -derivedDataPath {{derived}} \
      MARKETING_VERSION={{version}} \
      CURRENT_PROJECT_VERSION=$(git rev-list --count HEAD) -skipPackagePluginValidation -quiet build
    identity=$("{{just_executable()}}" --justfile "{{justfile()}}" _mac-client-identity); \
      codesign --force --sign "$identity" {{derived}}/Build/Products/Release/Domovoi.app; \
      codesign --verify --deep --strict {{derived}}/Build/Products/Release/Domovoi.app
    @"{{just_executable()}}" --justfile "{{justfile()}}" _mac-register "{{derived}}/Build/Products/Release/Domovoi.app"
    @"{{just_executable()}}" --justfile "{{justfile()}}" tray-app "{{derived}}/Build/Products/Release" true aarch64-apple-darwin
    @lipo -archs {{derived}}/Build/Products/Release/Domovoi.app/Contents/MacOS/Domovoi
    @lipo -archs "{{derived}}/Build/Products/Release/Domovoi Server.app/Contents/MacOS/domovoi-server"

# The release apps as dist/Domovoi.app and dist/Domovoi Server.app, a .zip each and one drag-to-Applications .dmg
mac-package: lfs-check mac-release
    rm -rf {{dist}}/*.app {{dist}}/*-macos.zip {{dist}}/*-macos.dmg {{dist}}/dmg && mkdir -p {{dist}}/dmg
    ditto {{derived}}/Build/Products/Release/Domovoi.app {{dist}}/Domovoi.app
    ditto "{{derived}}/Build/Products/Release/Domovoi Server.app" "{{dist}}/Domovoi Server.app"
    @"{{just_executable()}}" --justfile "{{justfile()}}" _mac-register {{dist}}/Domovoi.app
    ditto -c -k --keepParent {{dist}}/Domovoi.app {{dist}}/Domovoi-{{version}}-macos.zip
    ditto -c -k --keepParent "{{dist}}/Domovoi Server.app" {{dist}}/Domovoi-Server-{{version}}-macos.zip
    ditto {{dist}}/Domovoi.app {{dist}}/dmg/Domovoi.app
    ditto "{{dist}}/Domovoi Server.app" "{{dist}}/dmg/Domovoi Server.app"
    ln -s /Applications {{dist}}/dmg/Applications
    hdiutil create -quiet -volname "Domovoi {{version}}" -srcfolder {{dist}}/dmg -ov -format UDZO {{dist}}/Domovoi-{{version}}-macos.dmg
    # The staging copies would stay registered with Launch Services after they're gone.
    {{lsregister}} -u {{dist}}/dmg/Domovoi.app "{{dist}}/dmg/Domovoi Server.app" 2>/dev/null || true
    rm -rf {{dist}}/dmg
    @ls -lh {{dist}}
    @"{{just_executable()}}" --justfile "{{justfile()}}" size-report

# The Mac apps' executables segment by segment, against their budgets (tools/size-report.py)
size-report:
    python3 tools/size-report.py "{{dist}}/Domovoi.app={{client_budget}}" "{{dist}}/Domovoi Server.app={{server_budget}}"

# The server's code by crate: a release build that keeps its symbols, in its own target folder
size-crates n="30":
    CARGO_PROFILE_RELEASE_STRIP=none cargo build -q --release -p domovoi-server --target aarch64-apple-darwin --target-dir target/size
    python3 tools/size-report.py --crates {{n}} target/size/aarch64-apple-darwin/release/domovoi-server

# Tell Launch Services about a freshly built app. xcodebuild rewrites what's
# inside the bundle but not the bundle folder's date, which is what the icon
# cache goes by, so without the touch the Dock keeps an older icon.
_mac-register app:
    touch "{{app}}"
    {{lsregister}} -f "{{app}}"

# Fix a Dock or Finder showing the wrong Domovoi icon: forget copies of the app that are gone, register the builds again, restart the Dock
mac-icon-reset:
    #!/usr/bin/env bash
    set -euo pipefail
    # Blocks of `lsregister -dump` are separated by dashed lines; keep the
    # paths of those whose identifier is one of the apps.
    {{lsregister}} -dump | awk '
      /^-+$/ { if (id ~ /^dev\.domovoi\.(Domovoi|Domovoi\.debug|server)$/ && path != "") print path; id = ""; path = ""; next }
      /^identifier:/ { id = $2 }
      /^path:/ { sub(/^path: +/, ""); sub(/ \(0x[0-9a-f]+\)$/, ""); path = $0 }
    ' | sort -u | while IFS= read -r app; do
      if [[ ! -e "$app" ]]; then
        echo "forgetting $app"
        {{lsregister}} -u "$app" 2>/dev/null || true
      fi
    done
    for app in "{{derived}}/Build/Products/Debug/Domovoi.app" "{{derived}}/Build/Products/Release/Domovoi.app" "{{dist}}/Domovoi.app"; do
      if [[ -d "$app" ]]; then touch "$app"; {{lsregister}} -f "$app"; echo "registered $app"; fi
    done
    killall Dock 2>/dev/null || true

# --- Android app ---
# Needs Android Studio (its JDK and SDK; `ANDROID_HOME` defaults to
# ~/Library/Android/sdk), the NDK below, boltffi and the Rust targets
# aarch64-linux-android and x86_64-linux-android.

# Kotlin types for the core's events, effects and view model
android-typegen:
    cargo run -q -p domovoi_core --bin codegen --features codegen -- --kotlin {{droid}}/shared-types
    rm -f {{droid}}/shared-types/build.gradle.kts

# The core as JNI libraries (arm64 and x86_64) with its Kotlin bindings
android-ffi release="false":
    cd apps/domovoi-core && ANDROID_NDK_HOME={{android_ndk}} boltffi pack android {{ if release == "true" { "--release" } else { "" } }}

# Types and native libraries (run after changing the core)
android-generate release="false": android-typegen (android-ffi release)

# Android debug APK. Pass arm64-v8a or x86_64 for a smaller local install.
android-build abi="all": android-generate
    cd {{droid}} && JAVA_HOME="{{android_jdk}}" ./gradlew -q :app:assembleDebug -PdomovoiAbi="{{abi}}"
    @echo "{{droid}}/app/build/outputs/apk/debug/app-debug.apk"

# Kotlin unit tests for the shell (effects, store, the core's thread and render coalescing)
android-test: android-generate
    cd {{droid}} && JAVA_HOME="{{android_jdk}}" ./gradlew -q :app:testDebugUnitTest

# Build, install and launch on the emulator `avd` (booted when needed)
android-run avd=avd: android-build
    {{adb}} get-state >/dev/null 2>&1 || ({{android_sdk}}/emulator/emulator -avd {{avd}} -no-snapshot-save >/dev/null 2>&1 &)
    {{adb}} wait-for-device
    until [ "$({{adb}} shell getprop sys.boot_completed | tr -d '\r')" = "1" ]; do sleep 1; done
    {{adb}} install -r {{droid}}/app/build/outputs/apk/debug/app-debug.apk
    {{adb}} shell am start -n {{android_id}}/.MainActivity

# Build, install and launch on a USB device (`adb devices` lists serials)
android-device serial: android-build
    {{adb}} -s {{serial}} install -r {{droid}}/app/build/outputs/apk/debug/app-debug.apk
    {{adb}} -s {{serial}} shell am start -n {{android_id}}/.MainActivity

# Pair the emulator with a running server over iroh (opens a fresh domovoi:// link)
android-pair home=home:
    link=$(cargo run -q -p domovoi-server -- --home-dir {{home}} --port {{port}} pair --remote | sed -n 's/^link: //p'); \
    {{adb}} shell am start -a android.intent.action.VIEW -d "'$link'" {{android_id}}

# --- server packaging ---

# Release build (arm64) of domovoi-server in dist/
server-package: lfs-check
    cargo build --release -p domovoi-server --target aarch64-apple-darwin
    mkdir -p {{dist}}
    cp target/aarch64-apple-darwin/release/domovoi-server {{dist}}/domovoi-server
    codesign --force --sign - {{dist}}/domovoi-server
    tar -C {{dist}} -czf {{dist}}/domovoi-server-{{version}}-macos.tar.gz domovoi-server
    @lipo -archs {{dist}}/domovoi-server

# Headless Linux release build of domovoi-server in dist/ (cross-built with zig, glibc 2.34+; `TARGET=aarch64-unknown-linux-gnu` for ARM)
server-package-linux: lfs-check
    #!/usr/bin/env bash
    set -euo pipefail
    bin=$(tools/cross/linux.sh)
    arch=$(sed 's/-.*//' <<<"${TARGET:-x86_64-unknown-linux-gnu}")
    mkdir -p {{dist}}/linux-$arch
    cp "$bin" {{dist}}/linux-$arch/domovoi-server
    tar -C {{dist}}/linux-$arch -czf {{dist}}/domovoi-server-{{version}}-linux-$arch.tar.gz domovoi-server
    ls -lh {{dist}}/domovoi-server-{{version}}-linux-$arch.tar.gz

# --- Linux server (over ssh; DOMOVOI_LINUX_HOST or `just linux_host=user@host …` picks the machine) ---

# Cross-build the headless server and install it on the Linux machine as a systemd user service, then wait until it is up
linux-deploy:
    tools/linux/deploy.sh {{linux_host}}

# A pairing link and QR code (drawn here) for the server on the Linux machine, valid `ttl` minutes; `--light` for a light terminal
linux-pair ttl="10" *args="":
    ssh {{linux_host}} '~/.local/bin/domovoi-server pair --remote --ttl-minutes {{ttl}} {{args}}'

# Any domovoi-server subcommand on the Linux machine: `just linux remote status`, `just linux service status`, `just linux stop`
linux *args:
    ssh {{linux_host}} '~/.local/bin/domovoi-server {{args}}'

# Follow the server's log on the Linux machine
linux-logs:
    ssh -t {{linux_host}} 'journalctl --user -u domovoi-server -f -n 100'

# "Domovoi Server.app" in `out`: the server with its status bar icon, as something to double-click (no Dock icon).
# `release=false` builds debug; `target` cross-builds (`aarch64-apple-darwin`). The Mac app recipes build it too
tray-app out=dist release="true" target="":
    #!/usr/bin/env bash
    set -euo pipefail
    profile=debug; flag=
    if [[ "{{release}}" == true ]]; then profile=release; flag=--release; fi
    app="{{out}}/Domovoi Server.app"
    rm -rf "$app" && mkdir -p "$app/Contents/MacOS"
    if [[ -n "{{target}}" ]]; then
      cargo build $flag -p domovoi-server --target {{target}}
      cp target/{{target}}/$profile/domovoi-server "$app/Contents/MacOS/domovoi-server"
    else
      cargo build $flag -p domovoi-server
      cp target/$profile/domovoi-server "$app/Contents/MacOS/domovoi-server"
    fi
    sed -e 's/@VERSION@/{{version}}/' tools/tray-Info.plist > "$app/Contents/Info.plist"
    # The icon (apps/server/icon/AppIcon.icon, from tools/icons.py) needs Xcode's actool.
    mkdir -p "$app/Contents/Resources"
    if xcrun --find actool >/dev/null 2>&1; then
      xcrun actool apps/server/icon/AppIcon.icon --compile "$app/Contents/Resources" --platform macosx \
        --minimum-deployment-target 13.0 --app-icon AppIcon --output-partial-info-plist /dev/null \
        --errors --output-format human-readable-text >/dev/null
    else
      echo "warning: no actool (Xcode), so Domovoi Server.app has no icon" >&2
    fi
    # macOS keeps folder permissions (Documents, Downloads) per signature. An
    # ad-hoc one changes with every build, so each rebuild asks again and
    # blocks the server on the prompt; a certificate keeps them.
    # `DOMOVOI_SIGN_IDENTITY=-` signs ad hoc.
    identity="${DOMOVOI_SIGN_IDENTITY:-}"
    if [[ -z "$identity" ]]; then
      ids=$(security find-identity -v -p codesigning 2>/dev/null || true)
      identity=$(grep -F "Developer ID Application:" <<<"$ids" | grep -F "($DEVELOPMENT_TEAM)" | head -1 | awk '{print $2}' || true)
      [[ -n "$identity" ]] || identity=$(grep -F "Apple Development:" <<<"$ids" | head -1 | awk '{print $2}' || true)
      [[ -n "$identity" ]] || identity=-
    fi
    codesign --force --deep --sign "$identity" "$app"
    echo "$app"

# App and server, ready to hand out
package: mac-package server-package
    cd {{dist}} && shasum -a 256 *.zip *.dmg *.tar.gz > SHA256SUMS
    @cat {{dist}}/SHA256SUMS

# Redraw the app icons (tools/icons.py); `export` also renders PNGs and the server's Windows .ico (needs Xcode 26 and Pillow),
# which are kept in Git LFS
icons *args:
    python3 tools/icons.py {{args}}
    @case " {{args}} " in *" export "*) "{{just_executable()}}" --justfile "{{justfile()}}" lfs-check ;; esac

# Remove build products (keeps the Rust cache)
clean:
    rm -rf {{dist}} {{derived}} {{mac}}/generated {{mac}}/Domovoi.xcodeproj {{mac}}/DomovoiMacRelease.xcodeproj {{droid}}/shared-types {{droid}}/shared/src/main/kotlin {{droid}}/shared/src/main/jniLibs {{droid}}/app/build {{droid}}/shared/build

# --- Desktop app (Iced: Linux, Windows, macOS) ---

# Run the Iced desktop app (release build)
desktop:
    cargo run -p domovoi-iced --release

# Run the Iced desktop app (debug build, quicker compile while iterating)
desktop-dev:
    cargo run -p domovoi-iced

# Unit tests and headless Iced UI snapshot flows (snapshots in target/ui-snapshots/)
desktop-test:
    cargo test -p domovoi-iced

# Package the Iced desktop app for this OS into dist/: .app/.zip (macOS), .tar.gz with .desktop (Linux), or .exe (Windows, which registers domovoi:// itself on first run)
desktop-package: lfs-check
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release -p domovoi-iced
    bin=target/release/domovoi-desktop
    iced=apps/domovoi-iced
    mkdir -p {{dist}}
    case "{{os()}}" in
      macos)
        app="{{dist}}/Domovoi Desktop.app"
        rm -rf "$app" && mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
        cp "$bin" "$app/Contents/MacOS/domovoi-desktop"
        sed -e "s/@VERSION@/{{version}}/" -e "s/@BUILD@/$(git rev-list --count HEAD)/" \
          "$iced/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
        if xcrun --find actool >/dev/null 2>&1; then
          xcrun actool apps/domovoi-apple/Common/AppIcon.icon --compile "$app/Contents/Resources" --platform macosx \
            --minimum-deployment-target 13.0 --app-icon AppIcon --output-partial-info-plist /dev/null \
            --errors --output-format human-readable-text >/dev/null
        fi
        codesign --force --sign - "$app"
        @"{{just_executable()}}" --justfile "{{justfile()}}" _mac-register "$app" || true
        rm -f "{{dist}}/Domovoi-Desktop-{{version}}-macos.zip"
        ditto -c -k --keepParent "$app" "{{dist}}/Domovoi-Desktop-{{version}}-macos.zip"
        echo "$app"
        echo "{{dist}}/Domovoi-Desktop-{{version}}-macos.zip"
        ;;
      linux)
        name="domovoi-desktop-{{version}}-linux-$(uname -m)"
        stage="{{dist}}/$name"
        rm -rf "$stage" && mkdir -p "$stage/bin" "$stage/share/applications" "$stage/share/icons/hicolor/256x256/apps"
        cp "$bin" "$stage/bin/"
        cp "$iced/packaging/linux/domovoi.desktop" "$stage/share/applications/"
        cp "$iced/assets/app-icon-256.png" "$stage/share/icons/hicolor/256x256/apps/domovoi.png"
        install -m 755 "$iced/packaging/linux/install.sh" "$stage/install.sh"
        tar -C {{dist}} -czf "{{dist}}/$name.tar.gz" "$name"
        rm -rf "$stage"
        echo "{{dist}}/$name.tar.gz"
        ;;
      windows)
        cp "$bin.exe" "{{dist}}/domovoi-desktop-{{version}}-windows.exe"
        echo "{{dist}}/domovoi-desktop-{{version}}-windows.exe"
        ;;
    esac
