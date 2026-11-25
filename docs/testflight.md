# TestFlight from this Mac

`just testflight` archives the iPhone app (with its widget and share
extensions), signs it for distribution and uploads it to App Store Connect.
`just testflight-validate` does the same archive and `.ipa` export, then has
App Store Connect validate it without uploading.

```sh
just testflight-validate     # dry run: archive, export, validate
just testflight              # upload
just testflight 0.2.0        # upload with another marketing version than Cargo.toml's
BUILD=120 just testflight    # upload with an explicit build number
```

The script is `tools/ios/testflight.sh`.

## Version and build number

Nothing in the repository is edited. The marketing version
(`CFBundleShortVersionString`) is the workspace version in `Cargo.toml`, or the
argument. The build number (`CFBundleVersion`) is `git rev-list --count HEAD`,
or `BUILD`. Both are passed to xcodebuild, so the app and both extensions get
the same values. App Store Connect refuses a build number it has already seen
for a version, so a second upload from the same commit needs a higher `BUILD`.

## One-time setup

1. **App record.** In App Store Connect → Apps → New App: iOS, name "Domovoi",
   bundle ID `com.crayen.domovoi`. If the bundle ID isn't listed, register it
   under Certificates, IDs & Profiles first. Automatic signing registers the
   extensions (`com.crayen.domovoi.widgets`, `com.crayen.domovoi.share`) and
   the app group `group.com.crayen.domovoi` if the key may.
2. **API key.** App Store Connect → Users and Access → Integrations → App Store
   Connect API → generate a key with the **Admin** role (Xcode's automatic
   signing uses it to create the distribution certificate and profiles; App
   Manager is enough once they exist). Put `AuthKey_<KEYID>.p8` in
   `~/.appstoreconnect/private_keys/`, or set `ASC_KEY_PATH`.
3. **`.env`** (gitignored; see `.env.example`), or the same variables in the
   environment:
   ```
   ASC_KEY_ID=<key id>
   ASC_ISSUER_ID=<issuer id, shown above the keys list>
   ```
4. **Testers.** In the app → TestFlight, create an internal group. External
   groups need a Beta App Review per version.

## What the script does

1. Loads `.env` and checks that the API key exists.
2. Runs `just lfs-check`, and asks before building uncommitted changes
   (`FORCE=1` skips the question; without a terminal it fails instead).
3. Generates the release core and the Xcode project (`just mac-generate true`).
4. Archives the `DomovoiIOS` scheme (Release, `generic/platform=iOS`) into
   `dist/testflight/DomovoiIOS.xcarchive`.
5. Exports with `apps/domovoi-apple/ExportOptions-testflight.plist`
   (`app-store-connect`, `destination` `upload`): Xcode signs for distribution
   and uploads in one step. The distribution profile turns `aps-environment`
   into `production`.
6. Copies the archive's `dSYMs/` to `dist/testflight/dsyms/<version>-<build>/`.

Processing takes about 5–30 minutes, then the build appears under TestFlight.
`ITSAppUsesNonExemptEncryption = NO` answers the export-compliance question.

## Troubleshooting

- **"No profiles for … were found" / signing errors:** the key needs the Admin
  role, and the bundle IDs must belong to team `DEVELOPMENT_TEAM`
  (`LALG9U2L97`).
- **"The bundle version must be higher":** re-run with a `BUILD` above the
  last uploaded build.
- **"No suitable application records were found"** or, from
  `testflight-validate`, **"Cannot determine the Apple ID from Bundle ID"**:
  create the app record.
