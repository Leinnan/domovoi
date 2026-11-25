#!/usr/bin/env bash
# Archives the iPhone app (DomovoiIOS with its widget and share extensions),
# signs it for distribution and uploads it to TestFlight (or, with
# --validate-only, exports an .ipa and validates it with App Store Connect
# without uploading). See docs/testflight.md.
#
#   tools/ios/testflight.sh [--validate-only] [VERSION]
#
# VERSION sets CFBundleShortVersionString (default: DOMOVOI_VERSION, which the
# justfile sets from Cargo.toml). The build number is the commit count;
# BUILD=n overrides it. Needs ASC_KEY_ID / ASC_ISSUER_ID (environment or .env)
# and the key at ~/.appstoreconnect/private_keys/AuthKey_<ASC_KEY_ID>.p8 (or
# ASC_KEY_PATH). FORCE=1 skips the uncommitted-changes prompt.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

APPLE="apps/domovoi-apple"
EXPORT_OPTIONS="$APPLE/ExportOptions-testflight.plist"
OUT="dist/testflight"
ARCHIVE="$OUT/DomovoiIOS.xcarchive"
JUST="${JUST:-just}"

VALIDATE_ONLY=0
VERSION=""
for arg in "$@"; do
    case "$arg" in
        --validate-only) VALIDATE_ONLY=1 ;;
        -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
        -*) echo "error: unknown option $arg" >&2; exit 2 ;;
        *) VERSION="$arg" ;;
    esac
done

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
fail() { echo "error: $*" >&2; exit 1; }

# ── 1. Secrets ──────────────────────────────────────────────────────────────
step "Loading App Store Connect API key"
if [ -f .env ]; then
    set -a
    # shellcheck disable=SC1091
    source .env
    set +a
fi
[ -n "${ASC_KEY_ID:-}" ] || fail "ASC_KEY_ID is not set (add it to .env; see docs/testflight.md)"
[ -n "${ASC_ISSUER_ID:-}" ] || fail "ASC_ISSUER_ID is not set (add it to .env; see docs/testflight.md)"
KEY="${ASC_KEY_PATH:-$HOME/.appstoreconnect/private_keys/AuthKey_${ASC_KEY_ID}.p8}"
[ -f "$KEY" ] || fail "API key not found at $KEY (see docs/testflight.md)"
AUTH=(-allowProvisioningUpdates -authenticationKeyPath "$KEY"
      -authenticationKeyID "$ASC_KEY_ID" -authenticationKeyIssuerID "$ASC_ISSUER_ID")
echo "key $ASC_KEY_ID"

# ── 2. Preflight ────────────────────────────────────────────────────────────
step "Preflight checks"
"$JUST" lfs-check
if [ -n "$(git status --porcelain)" ] && [ "${FORCE:-0}" != "1" ]; then
    echo "warning: the working tree has uncommitted changes; they will be in this build" >&2
    if [ -t 0 ]; then
        read -r -p "Continue anyway? [y/N] " answer
        [[ "$answer" =~ ^[Yy]$ ]] || fail "aborted"
    else
        fail "uncommitted changes (re-run with FORCE=1 to build them anyway)"
    fi
fi

# ── 3. Version and build number ─────────────────────────────────────────────
step "Version and build number"
VERSION="${VERSION:-${DOMOVOI_VERSION:-}}"
[ -n "$VERSION" ] || VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
[[ "$VERSION" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]] || fail "version '$VERSION' is not N, N.N or N.N.N"
BUILD="${BUILD:-$(git rev-list --count HEAD)}"
[[ "$BUILD" =~ ^[0-9]+$ ]] || fail "build number '$BUILD' is not a number"
echo "$VERSION ($BUILD)"

# ── 4. Generate ─────────────────────────────────────────────────────────────
step "Generating the release core and the Xcode project"
"$JUST" mac-generate true

# ── 5. Archive ──────────────────────────────────────────────────────────────
step "Archiving DomovoiIOS (Release)"
rm -rf "$ARCHIVE"
mkdir -p "$OUT"
xcodebuild -project "$APPLE/Domovoi.xcodeproj" -scheme DomovoiIOS -configuration Release \
    -destination 'generic/platform=iOS' -derivedDataPath "$APPLE/build" \
    -archivePath "$ARCHIVE" "${AUTH[@]}" \
    MARKETING_VERSION="$VERSION" CURRENT_PROJECT_VERSION="$BUILD" \
    -skipPackagePluginValidation -quiet clean archive
[ -d "$ARCHIVE" ] || fail "archive not produced"

# ── 6. Export (+ upload or validate) ────────────────────────────────────────
rm -rf "$OUT/export"
if [ "$VALIDATE_ONLY" = "1" ]; then
    step "Exporting .ipa and validating with App Store Connect (no upload)"
    options="$OUT/ExportOptions-export.plist"
    cp "$EXPORT_OPTIONS" "$options"
    plutil -replace destination -string export "$options"
    xcodebuild -exportArchive -archivePath "$ARCHIVE" -exportOptionsPlist "$options" \
        -exportPath "$OUT/export" "${AUTH[@]}" -quiet
    ipa="$(find "$OUT/export" -name '*.ipa' -print -quit)"
    [ -n "$ipa" ] || fail "no .ipa exported"
    API_PRIVATE_KEYS_DIR="$(dirname "$KEY")" xcrun altool --validate-app -f "$ipa" -t ios \
        --apiKey "$ASC_KEY_ID" --apiIssuer "$ASC_ISSUER_ID"
    echo
    echo "Validated Domovoi $VERSION ($BUILD): $ipa"
    exit 0
fi

step "Exporting and uploading to App Store Connect"
xcodebuild -exportArchive -archivePath "$ARCHIVE" -exportOptionsPlist "$EXPORT_OPTIONS" \
    -exportPath "$OUT/export" "${AUTH[@]}"

# The next run deletes the archive; a crash log from this build still needs
# its symbols.
DSYMS="$OUT/dsyms/$VERSION-$BUILD"
rm -rf "$DSYMS"
mkdir -p "$OUT/dsyms"
cp -R "$ARCHIVE/dSYMs" "$DSYMS"

cat <<EOF

Uploaded Domovoi $VERSION ($BUILD).
  archive: $ARCHIVE
  dSYMs:   $DSYMS
App Store Connect processes the build (usually 5-30 min); then it appears under TestFlight.
EOF
