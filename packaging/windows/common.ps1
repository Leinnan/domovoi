# What package.ps1 does, step by step (after wihaister's packaging and
# lwa_fm's). Paths: the release build goes to target\windows-package (its own
# target folder: the static C runtime changes every crate's flags), the
# installers to dist\windows.

$script:DomovoiRepo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$script:DomovoiWork = Join-Path $script:DomovoiRepo "target\windows-package"
$script:DomovoiRelease = Join-Path $script:DomovoiWork "release"
$script:DomovoiDist = Join-Path $script:DomovoiRepo "dist\windows"
# What cargo keeps in a profile folder: not part of the runtime.
$script:CargoOwn = @(".fingerprint", "build", "deps", "examples", "incremental", ".cargo-lock")

function Invoke-DomovoiChecked([string]$What, [scriptblock]$Command) {
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
}

function Reset-DomovoiDirectory([string]$Path) {
    # Only ever this script's own folders, under target\windows-package.
    if (-not $Path.StartsWith($script:DomovoiWork, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to clear ${Path}: it is outside $script:DomovoiWork"
    }
    if (Test-Path -LiteralPath $Path) { Remove-Item -LiteralPath $Path -Recurse -Force }
    New-Item -ItemType Directory -Path $Path -Force | Out-Null
}

# The workspace version (Cargo.toml [workspace.package]); DOMOVOI_RELEASE_VERSION overrides it.
function Get-DomovoiVersion {
    $version = $env:DOMOVOI_RELEASE_VERSION
    if ([string]::IsNullOrWhiteSpace($version)) {
        $manifest = Get-Content -Raw (Join-Path $script:DomovoiRepo "Cargo.toml")
        if ($manifest -notmatch '(?ms)^\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]+)"') { throw "No workspace version in Cargo.toml" }
        $version = $Matches[1]
    }
    if ($version -notmatch '^\d+\.\d+\.\d+$') { throw "The installer version must be x.y.z, not '$version'" }
    return $version
}

function Invoke-DomovoiReleaseBuild([string[]]$Packages) {
    Write-Host "==> Building $($Packages -join ', ') (x64 release, static C runtime)"
    # What an earlier build left beside the exe may be another revision of the
    # Windows App Runtime: cleared, and staged again by the build script.
    if (Test-Path -LiteralPath $script:DomovoiRelease) {
        Get-ChildItem -LiteralPath $script:DomovoiRelease -Force | Where-Object { $_.Name -notin $script:CargoOwn } |
            ForEach-Object { Remove-Item -LiteralPath $_.FullName -Recurse -Force }
    }
    (Get-Item -LiteralPath (Join-Path $script:DomovoiRepo "apps\domovoi-windows\build.rs")).LastWriteTime = Get-Date
    $saved = @{ RUSTFLAGS = $env:RUSTFLAGS; RUSTC_WRAPPER = $env:RUSTC_WRAPPER }
    $env:RUSTFLAGS = "-C target-feature=+crt-static"
    # sccache can pass the Windows command-line limit on the big links.
    $env:RUSTC_WRAPPER = ""
    try {
        Push-Location $script:DomovoiRepo
        $cargoArgs = @("build", "--locked", "--release", "--target-dir", $script:DomovoiWork)
        foreach ($p in $Packages) { $cargoArgs += @("--package", $p) }
        Invoke-DomovoiChecked "The release build" { cargo @cargoArgs }
    } finally {
        Pop-Location
        $env:RUSTFLAGS = $saved.RUSTFLAGS
        $env:RUSTC_WRAPPER = $saved.RUSTC_WRAPPER
    }
}

# The WinUI app's runtime: everything the build script staged beside it.
function Copy-DomovoiRuntime([string]$To) {
    foreach ($required in @("Microsoft.WindowsAppRuntime.dll", "Microsoft.ui.xaml.dll", "resources.pri")) {
        if (-not (Test-Path -LiteralPath (Join-Path $script:DomovoiRelease $required))) { throw "The build has no $required beside Domovoi.exe" }
    }
    Get-ChildItem -LiteralPath $script:DomovoiRelease -Force | Where-Object {
        # Lock files (cargo's, windows-reactor-setup's) start with a dot.
        $_.Name -notin $script:CargoOwn -and -not $_.Name.StartsWith(".") -and $_.Extension -notin @(".exe", ".pdb", ".d", ".rlib", ".lib", ".exp")
    } | ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $To -Recurse }
    $files = @(Get-ChildItem -LiteralPath $To -Recurse -File)
    Write-Host "    runtime: $($files.Count) files, $([math]::Round(($files | Measure-Object Length -Sum).Sum / 1MB)) MB"
}

# The image names none of `Names` (the DLLs a static C runtime does without).
function Assert-DomovoiNoReference([string]$Path, [string[]]$Names) {
    $image = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($Path))
    foreach ($name in $Names) {
        if ($image.IndexOf($name, [StringComparison]::OrdinalIgnoreCase) -ge 0) { throw "$(Split-Path -Leaf $Path) needs $name" }
    }
}

function Test-DomovoiSigning { -not [string]::IsNullOrWhiteSpace($env:DOMOVOI_SIGN_CERT_THUMBPRINT) }

function Invoke-DomovoiSign([string]$Path) {
    $signtool = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($null -eq $signtool) {
        $kit = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue | Sort-Object FullName | Select-Object -Last 1
        if ($null -eq $kit) { throw "signtool.exe not found: install the Windows SDK's signing tools" }
        $signtool = $kit.FullName
    } else { $signtool = $signtool.Source }
    $signArgs = @("sign", "/fd", "SHA256", "/sha1", $env:DOMOVOI_SIGN_CERT_THUMBPRINT)
    if (-not [string]::IsNullOrWhiteSpace($env:DOMOVOI_SIGN_TIMESTAMP_URL)) { $signArgs += @("/tr", $env:DOMOVOI_SIGN_TIMESTAMP_URL, "/td", "SHA256") }
    Invoke-DomovoiChecked "signtool for $Path" { & $signtool @signArgs $Path }
}

# The pinned WiX (`dotnet tool restore`), never one on PATH: CI builds with it.
function Invoke-DomovoiWix([string[]]$WixArgs) {
    Push-Location $script:DomovoiWork
    try {
        Invoke-DomovoiChecked "wix $($WixArgs[0])" { dotnet tool run wix -- @WixArgs }
    } finally { Pop-Location }
}

function New-DomovoiMsi([string]$Msi, [string]$Flavor, [string]$Version, [string]$BinDir, [string]$RuntimeDir, [string]$IconFile) {
    Write-Host "==> Building $(Split-Path -Leaf $Msi)"
    New-Item -ItemType Directory -Path $script:DomovoiWork -Force | Out-Null
    # The UI and util extensions of the same WiX, kept under target (`.wix`).
    $wixVersion = ((& dotnet tool run wix -- --version) -join ' ') -replace '^(\d+\.\d+\.\d+).*', '$1'
    Invoke-DomovoiWix @("extension", "add", "WixToolset.UI.wixext/$wixVersion", "WixToolset.Util.wixext/$wixVersion")
    Remove-Item -LiteralPath $Msi -Force -ErrorAction SilentlyContinue
    Invoke-DomovoiWix @("build", (Join-Path $PSScriptRoot "domovoi.wxs"), "-o", $Msi, "-arch", "x64",
        "-ext", "WixToolset.UI.wixext", "-ext", "WixToolset.Util.wixext",
        "-d", "Version=$Version", "-d", "Flavor=$Flavor", "-d", "BinDir=$BinDir", "-d", "RuntimeDir=$RuntimeDir", "-d", "IconFile=$IconFile")
    # WiX takes each file's language from its version resource; the runtime's
    # name dozens, or ones the installer doesn't know, and validation fails.
    # Nothing is installed by language: every file is neutral.
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $database = $installer.OpenDatabase($Msi, 1)
    try {
        $view = $database.OpenView('UPDATE `File` SET `Language` = 0')
        try { $view.Execute() | Out-Null } finally { $view.Close() | Out-Null; [void][Runtime.InteropServices.Marshal]::ReleaseComObject($view) }
        $database.Commit()
    } finally {
        [void][Runtime.InteropServices.Marshal]::ReleaseComObject($database)
        [void][Runtime.InteropServices.Marshal]::ReleaseComObject($installer)
    }
    # ICE61, ICE40: same-version upgrades and REINSTALLMODE are meant.
    # ICE38, ICE64, ICE91: a per-user install into the profile, on purpose.
    Invoke-DomovoiWix @("msi", "validate", "-sice", "ICE61", "-sice", "ICE40", "-sice", "ICE38", "-sice", "ICE64", "-sice", "ICE91", $Msi)
}

# The MSI unpacked as an administrator would (`msiexec /a`): the folder with the programs.
function Expand-DomovoiMsi([string]$Msi, [string]$Into) {
    Reset-DomovoiDirectory $Into
    $unpack = Start-Process msiexec.exe -ArgumentList "/a `"$Msi`" /qn TARGETDIR=`"$Into`"" -Wait -PassThru
    if ($unpack.ExitCode -ne 0) { throw "Unpacking $Msi failed with exit code $($unpack.ExitCode)" }
    $exe = Get-ChildItem -LiteralPath $Into -Recurse -Filter domovoi-server.exe | Select-Object -First 1
    if ($null -eq $exe) { throw "The unpacked $Msi has no domovoi-server.exe" }
    return $exe.DirectoryName
}

function Test-DomovoiServerRuns([string]$Folder) {
    Write-Host "==> Starting the unpacked server"
    $out = & (Join-Path $Folder "domovoi-server.exe") --version
    if ($LASTEXITCODE -ne 0 -or -not ($out -join ' ')) { throw "The unpacked domovoi-server.exe did not say its version" }
    Write-Host "    $($out -join ' ')"
}

# Being alive proves little: a UI thread stuck in startup leaves a process with no window.
function Test-DomovoiClientOpens([string]$Folder) {
    Write-Host "==> Starting the unpacked app"
    if (-not ('DomovoiWindowProbe' -as [type])) {
        Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class DomovoiWindowProbe {
    delegate bool EnumProc(IntPtr window, IntPtr data);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr data);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr window, StringBuilder name, int length);
    public static bool HasWindow(uint process) {
        bool found = false;
        EnumWindows((window, data) => {
            uint owner; GetWindowThreadProcessId(window, out owner);
            if (owner == process && IsWindowVisible(window)) {
                var name = new StringBuilder(256); GetClassName(window, name, 256);
                if (name.ToString() == "WinUIDesktopWin32WindowClass") { found = true; return false; }
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@
    }
    # Its own data folder and memory-only secrets: the user's are not touched.
    $data = Join-Path $script:DomovoiWork "smoke-data"
    Reset-DomovoiDirectory $data
    # Nor does the unpacked copy register domovoi:// or its app id with Windows.
    $saved = @{ DOMOVOI_DATA = $env:DOMOVOI_DATA; DOMOVOI_STORE = $env:DOMOVOI_STORE; DOMOVOI_NO_REGISTER = $env:DOMOVOI_NO_REGISTER }
    $env:DOMOVOI_DATA = $data
    $env:DOMOVOI_STORE = "memory"
    $env:DOMOVOI_NO_REGISTER = "1"
    $said = Join-Path $data "stderr.txt"
    try {
        $app = Start-Process -FilePath (Join-Path $Folder "Domovoi.exe") -WorkingDirectory $Folder -PassThru -RedirectStandardError $said
    } finally {
        $env:DOMOVOI_DATA = $saved.DOMOVOI_DATA
        $env:DOMOVOI_STORE = $saved.DOMOVOI_STORE
        $env:DOMOVOI_NO_REGISTER = $saved.DOMOVOI_NO_REGISTER
    }
    try {
        $deadline = (Get-Date).AddSeconds(30)
        while (-not [DomovoiWindowProbe]::HasWindow([uint32]$app.Id)) {
            if ($app.HasExited) { throw "The unpacked app ended while starting, with code $($app.ExitCode): $(Get-Content -Raw -LiteralPath $said -ErrorAction SilentlyContinue)" }
            if ((Get-Date) -gt $deadline) { throw "The unpacked app opened no window within 30 seconds" }
            Start-Sleep -Milliseconds 250
        }
        if ($app.WaitForExit(3000)) { throw "The unpacked app ended after opening its window, with code $($app.ExitCode)" }
        Write-Host "    its window is open"
    } finally {
        if (-not $app.HasExited) { Stop-Process -Id $app.Id -Force; $app.WaitForExit() }
    }
}

function Write-DomovoiSums([string[]]$Files) {
    $sums = Join-Path $script:DomovoiDist "SHA256SUMS"
    $lines = foreach ($f in $Files) { "$((Get-FileHash -LiteralPath $f -Algorithm SHA256).Hash.ToLowerInvariant())  $(Split-Path -Leaf $f)" }
    Set-Content -LiteralPath $sums -Value $lines -Encoding ascii
    Write-Host "==> $sums"
}
