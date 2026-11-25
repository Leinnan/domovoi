# Builds Domovoi's Windows installers (`just windows-installers`):
#
#   dist\windows\Domovoi-<version>-windows-x64.msi         the WinUI app, the Iced app, the server
#   dist\windows\Domovoi-Server-<version>-windows-x64.msi  the server alone
#   dist\windows\SHA256SUMS
#
# Release builds with the C runtime linked in, so the apps need nothing
# installed beside them; the WinUI app carries its own Windows App Runtime
# (windows-reactor-setup stages it beside the exe). Each MSI is validated,
# unpacked the way an administrator would, and the unpacked programs are
# started: the WinUI app must open its window, the server must say its version.
#
# Needs the WiX pinned in .config\dotnet-tools.json (`dotnet tool restore`),
# MSVC and the Windows SDK. With DOMOVOI_SIGN_CERT_THUMBPRINT set (a
# certificate in the user's store), the programs and the MSIs are signed;
# DOMOVOI_SIGN_TIMESTAMP_URL adds an RFC 3161 timestamp.
[CmdletBinding()]
param(
    # full (everything) or server (the server alone); `both` builds the two.
    [ValidateSet("full", "server", "both")]
    [string]$Flavor = "both",
    # Skip starting the unpacked programs.
    [switch]$NoSmoke,
    # Package the release build already in target\windows-package.
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "common.ps1")

$version = Get-DomovoiVersion
Write-Host "==> Domovoi $version"
$flavors = if ($Flavor -eq "both") { @("full", "server") } else { @($Flavor) }
$packages = @("domovoi-server", "domovoi-server-launcher")
if ($flavors -contains "full") { $packages += @("domovoi-windows", "domovoi-iced") }

if (-not $SkipBuild) {
    Invoke-DomovoiReleaseBuild -Packages $packages
}

$bin = Join-Path $DomovoiWork "stage\bin"
$runtime = Join-Path $DomovoiWork "stage\runtime"
Reset-DomovoiDirectory $bin
Reset-DomovoiDirectory $runtime
$exes = @("domovoi-server.exe", "domovoi-server-launcher.exe")
if ($flavors -contains "full") { $exes += @("Domovoi.exe", "domovoi-desktop.exe") }
foreach ($exe in $exes) {
    $built = Join-Path $DomovoiRelease $exe
    if (-not (Test-Path -LiteralPath $built -PathType Leaf)) { throw "The build has no $exe" }
    # Linked with the C runtime: nothing to install beside it.
    Assert-DomovoiNoReference -Path $built -Names @("VCRUNTIME140.dll", "MSVCP140.dll", "Microsoft.WindowsAppRuntime.Bootstrap.dll")
    Copy-Item -LiteralPath $built -Destination $bin
}
if ($flavors -contains "full") {
    Copy-DomovoiRuntime -To $runtime
}
if (Test-DomovoiSigning) {
    Write-Host "==> Signing the programs"
    Get-ChildItem -LiteralPath $bin -Filter *.exe | ForEach-Object { Invoke-DomovoiSign $_.FullName }
}

New-Item -ItemType Directory -Path $DomovoiDist -Force | Out-Null
$built = @()
foreach ($f in $flavors) {
    $name = if ($f -eq "server") { "Domovoi-Server-$version-windows-x64.msi" } else { "Domovoi-$version-windows-x64.msi" }
    $msi = Join-Path $DomovoiDist $name
    $icon = Join-Path $DomovoiRepo $(if ($f -eq "server") { "apps/server/icon/domovoi-server.ico" } else { "apps/domovoi-iced/assets/app-icon.ico" })
    New-DomovoiMsi -Msi $msi -Flavor $f -Version $version -BinDir $bin -RuntimeDir $runtime -IconFile $icon
    if (Test-DomovoiSigning) { Invoke-DomovoiSign $msi }
    $installed = Expand-DomovoiMsi -Msi $msi -Into (Join-Path $DomovoiWork "extract-$f")
    $required = if ($f -eq "server") { @("domovoi-server.exe", "domovoi-server-launcher.exe") } else { @("Domovoi.exe", "domovoi-desktop.exe", "domovoi-server.exe", "domovoi-server-launcher.exe", "Microsoft.WindowsAppRuntime.dll", "Microsoft.ui.xaml.dll") }
    foreach ($file in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $installed $file) -PathType Leaf)) { throw "The unpacked $name has no $file" }
    }
    if (-not $NoSmoke) {
        Test-DomovoiServerRuns -Folder $installed
        if ($f -eq "full") { Test-DomovoiClientOpens -Folder $installed }
    }
    $built += $msi
}

Write-DomovoiSums -Files (Get-ChildItem -LiteralPath $DomovoiDist -Filter *.msi | ForEach-Object FullName)
foreach ($msi in $built) {
    $size = [math]::Round((Get-Item -LiteralPath $msi).Length / 1MB)
    Write-Host "==> $msi ($size MB)"
}
