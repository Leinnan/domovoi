# domovoi: the Rust server, the Crux client core and the macOS and iPhone apps.
# `just` lists the recipes. Packaging needs Xcode, xcodegen and boltffi
# (`cargo install boltffi_cli`); the Rust targets aarch64-apple-darwin and
# x86_64-apple-darwin give universal binaries.

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
lsregister := "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

# Signing team for the iPhone app; `DEVELOPMENT_TEAM=<team> just …` picks another.
export DEVELOPMENT_TEAM := env("DEVELOPMENT_TEAM", "LALG9U2L97")

default:
    @just --list --unsorted

# --- Rust ---

# Build everything (debug)
build:
    cargo build --workspace

# Every test; the schema checks also run when Node 26+ is on PATH (or in DOMOVOI_NODE)
test:
    DOMOVOI_NODE="${DOMOVOI_NODE:-$(command -v node || true)}" cargo test --workspace

# Every RPC method called for each answer and failure it can give, group by
# group (conformance/rpc-groups); six to ten minutes. `just rpc-groups vcs-status,vcs-refs` runs some
rpc-groups groups="":
    DOMOVOI_NODE="${DOMOVOI_NODE:-$(command -v node || true)}" {{ if groups == "" { "" } else { "DOMOVOI_GROUPS=" + groups } }} cargo test -p domovoi-server --test rpc_calls every_rpc_group_answers_as_recorded -- --ignored --nocapture

# Build everything and fail on any compiler warning or lint-gate finding
check:
    #!/usr/bin/env bash
    set -uo pipefail
    for cmd in "cargo build --workspace" "cargo test --workspace --no-run"; do
      out=$($cmd 2>&1) || { echo "$out"; exit 1; }
      if grep -E -A6 '^(warning|error)' <<<"$out"; then exit 1; fi
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

# The core as a Swift package (universal xcframework); `release=true` optimizes it
mac-ffi release="false":
    cd apps/domovoi-core && boltffi pack apple {{ if release == "true" { "--release" } else { "" } }}

# Generated sources and the Xcode project
mac-generate release="false": mac-typegen (mac-ffi release)
    cd {{mac}} && xcodegen -q

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

# Universal release build of the app and "Domovoi Server.app", signed ad hoc
mac-release: (mac-generate "true")
    xcodebuild -project {{mac}}/Domovoi.xcodeproj -scheme Domovoi -configuration Release -derivedDataPath {{derived}} \
      ARCHS="arm64 x86_64" ONLY_ACTIVE_ARCH=NO MARKETING_VERSION={{version}} \
      CURRENT_PROJECT_VERSION=$(git rev-list --count HEAD) -skipPackagePluginValidation -quiet build
    codesign --force --deep --sign - {{derived}}/Build/Products/Release/Domovoi.app
    @"{{just_executable()}}" --justfile "{{justfile()}}" _mac-register "{{derived}}/Build/Products/Release/Domovoi.app"
    @"{{just_executable()}}" --justfile "{{justfile()}}" tray-app "{{derived}}/Build/Products/Release" true true
    @lipo -archs {{derived}}/Build/Products/Release/Domovoi.app/Contents/MacOS/Domovoi
    @lipo -archs "{{derived}}/Build/Products/Release/Domovoi Server.app/Contents/MacOS/domovoi-server"

# The release apps as dist/Domovoi.app and dist/Domovoi Server.app, a .zip each and one drag-to-Applications .dmg
mac-package: mac-release
    rm -rf {{dist}}/Domovoi.app "{{dist}}/Domovoi Server.app" {{dist}}/dmg && mkdir -p {{dist}}/dmg
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

# Android app (debug APK)
android-build: android-generate
    cd {{droid}} && JAVA_HOME="{{android_jdk}}" ./gradlew -q :app:assembleDebug
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

# Universal release build of domovoi-server in dist/
server-package:
    cargo build --release -p domovoi-server --target aarch64-apple-darwin
    cargo build --release -p domovoi-server --target x86_64-apple-darwin
    mkdir -p {{dist}}
    lipo -create -output {{dist}}/domovoi-server target/aarch64-apple-darwin/release/domovoi-server target/x86_64-apple-darwin/release/domovoi-server
    codesign --force --sign - {{dist}}/domovoi-server
    tar -C {{dist}} -czf {{dist}}/domovoi-server-{{version}}-macos.tar.gz domovoi-server
    @lipo -archs {{dist}}/domovoi-server

# Headless Linux release build of domovoi-server in dist/ (cross-built with zig, glibc 2.34+; `TARGET=aarch64-unknown-linux-gnu` for ARM)
server-package-linux:
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
# `release=false` builds debug; `universal=true` gives arm64 and x86_64. The Mac app recipes build it too
tray-app out=dist release="true" universal="false":
    #!/usr/bin/env bash
    set -euo pipefail
    profile=debug; flag=
    if [[ "{{release}}" == true ]]; then profile=release; flag=--release; fi
    app="{{out}}/Domovoi Server.app"
    rm -rf "$app" && mkdir -p "$app/Contents/MacOS"
    if [[ "{{universal}}" == true ]]; then
      for target in aarch64-apple-darwin x86_64-apple-darwin; do
        cargo build $flag -p domovoi-server --target $target
      done
      lipo -create -output "$app/Contents/MacOS/domovoi-server" \
        target/aarch64-apple-darwin/$profile/domovoi-server target/x86_64-apple-darwin/$profile/domovoi-server
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

# Redraw the app icons (tools/icons.py); `export` also renders PNGs and the server's Windows .ico (needs Xcode 26 and Pillow)
icons *args:
    python3 tools/icons.py {{args}}

# Remove build products (keeps the Rust cache)
clean:
    rm -rf {{dist}} {{derived}} {{mac}}/generated {{mac}}/Domovoi.xcodeproj {{droid}}/shared-types {{droid}}/shared/src/main/kotlin {{droid}}/shared/src/main/jniLibs {{droid}}/app/build {{droid}}/shared/build
