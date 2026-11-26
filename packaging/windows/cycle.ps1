# Installs, upgrades, downgrades, repairs and removes the built MSIs for this
# user, checking each step (`just windows-installer-cycle`):
#
#   1. The full MSI at <version> with "Start at sign-in", then <version+1>
#      over it: one product, the new version, the choice and the user's data
#      kept.
#   2. <version> again: refused (no downgrade), <version+1> still there.
#   3. A deleted program file comes back with a repair (`msiexec /fa`).
#   4. The Desktop feature added afterwards, as Apps & features does it.
#   5. The server-only MSI beside the full one: refused.
#   6. Uninstall: the product, its files and its Run value go; the data stays.
#   7. The server-only MSI, then the full one: it takes the server-only one's
#      place and keeps "Start at sign-in".
#
# It refuses to run while Domovoi or Domovoi Server is installed, and leaves
# neither installed. The checks ask Windows Installer (not the registry) which
# products are there: in a terminal of the Claude desktop app, a child's view
# of HKCU\Software is a packaged app's.
[CmdletBinding()]
param(
    # Package the release build already in target\windows-package.
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "common.ps1")

$FullCode = "{C3A9E4F2-8D6B-4F1A-A7E5-1B2D3C4E5F60}"
$ServerCode = "{5B7F6D1E-2C4A-4E8B-9F3D-7A1C0E6B4D21}"
$Folder = Join-Path $env:LOCALAPPDATA "Programs\Domovoi"
$RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$Work = Join-Path $DomovoiRepo "target\windows-cycle"
$installer = New-Object -ComObject WindowsInstaller.Installer

function Get-Installed([string]$UpgradeCode) {
    @($installer.RelatedProducts($UpgradeCode))
}

function Get-InstalledVersion([string]$UpgradeCode) {
    $products = Get-Installed $UpgradeCode
    if ($products.Count -ne 1) { return "$($products.Count) products" }
    $installer.ProductInfo($products[0], "VersionString")
}

function Invoke-Msiexec([string]$Step, [string[]]$Arguments) {
    $log = Join-Path $Work "$Step.log"
    $run = Start-Process msiexec.exe -Wait -PassThru -ArgumentList ($Arguments + @("/qn", "/l*v", "`"$log`""))
    Write-Host "    msiexec $($Arguments -join ' ') -> $($run.ExitCode)"
    $run.ExitCode
}

function Assert-True([bool]$Condition, [string]$What) {
    if (-not $Condition) { throw "Failed: $What (logs: $Work)" }
    Write-Host "    ok: $What"
}

function Test-RunValue {
    $null -ne (Get-ItemProperty -LiteralPath $RunKey -Name "Domovoi Server" -ErrorAction SilentlyContinue)
}

function Remove-Everything {
    foreach ($code in @($FullCode, $ServerCode)) {
        foreach ($product in Get-Installed $code) {
            Invoke-Msiexec "cleanup-$product" @("/x", $product) | Out-Null
        }
    }
}

if ((Get-Installed $FullCode).Count -or (Get-Installed $ServerCode).Count) {
    throw "Domovoi is installed for this user: uninstall it first (this cycle installs and removes it several times)"
}

$version = Get-DomovoiVersion
$parts = $version.Split(".")
$next = "$($parts[0]).$($parts[1]).$([int]$parts[2] + 1)"
Reset-DomovoiDirectory $Work

Write-Host "==> Packaging $version and $next"
$packageArgs = @{ Flavor = "both" }
if ($SkipBuild) { $packageArgs.SkipBuild = $true }
& (Join-Path $PSScriptRoot "package.ps1") @packageArgs
$env:DOMOVOI_RELEASE_VERSION = $next
try {
    & (Join-Path $PSScriptRoot "package.ps1") -Flavor full -SkipBuild -NoSmoke
} finally {
    Remove-Item Env:\DOMOVOI_RELEASE_VERSION
}
$old = Join-Path $Work "Domovoi-$version.msi"
$new = Join-Path $Work "Domovoi-$next.msi"
$server = Join-Path $Work "Domovoi-Server-$version.msi"
Copy-Item -LiteralPath (Join-Path $DomovoiDist "Domovoi-$version-windows-x64.msi") -Destination $old
Move-Item -LiteralPath (Join-Path $DomovoiDist "Domovoi-$next-windows-x64.msi") -Destination $new -Force
Copy-Item -LiteralPath (Join-Path $DomovoiDist "Domovoi-Server-$version-windows-x64.msi") -Destination $server
# dist\ keeps only this version's installers, and their sums.
Write-DomovoiSums -Files (Get-ChildItem -LiteralPath $DomovoiDist -Filter *.msi | ForEach-Object FullName)

$data = Join-Path $env:APPDATA "domovoi-windows"
$marker = Join-Path $data "cycle-marker"
$hadData = Test-Path -LiteralPath $data
try {
    Write-Host "==> 1. Upgrade"
    Assert-True ((Invoke-Msiexec "1-install-old" @("/i", "`"$old`"", "ADDLOCAL=Client,Server,ServerAtSignIn")) -eq 0) "$version installs"
    Assert-True (Test-RunValue) "Start at sign-in is on"
    New-Item -ItemType Directory -Path $data -Force | Out-Null
    Set-Content -LiteralPath $marker -Value "kept"
    Assert-True ((Invoke-Msiexec "1-upgrade" @("/i", "`"$new`"")) -eq 0) "$next installs over it"
    Assert-True ((Get-InstalledVersion $FullCode) -eq $next) "one Domovoi, at $next"
    Assert-True (Test-RunValue) "Start at sign-in is still on"
    Assert-True (Test-Path -LiteralPath $marker) "the app's data is kept"
    Assert-True (Test-Path -LiteralPath (Join-Path $Folder "Domovoi.exe")) "Domovoi.exe is installed"

    Write-Host "==> 2. Downgrade"
    Assert-True ((Invoke-Msiexec "2-downgrade" @("/i", "`"$old`"")) -ne 0) "$version over $next is refused"
    Assert-True ((Get-InstalledVersion $FullCode) -eq $next) "$next is still installed"

    Write-Host "==> 3. Repair"
    $launcher = Join-Path $Folder "domovoi-server-launcher.exe"
    Remove-Item -LiteralPath $launcher
    Assert-True ((Invoke-Msiexec "3-repair" @("/fa", "`"$new`"")) -eq 0) "the repair runs"
    Assert-True (Test-Path -LiteralPath $launcher) "the deleted launcher is back"

    Write-Host "==> 4. A feature added later"
    $product = (Get-Installed $FullCode)[0]
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $Folder "domovoi-desktop.exe"))) "Domovoi Desktop is not installed by default"
    Assert-True ((Invoke-Msiexec "4-add-desktop" @("/i", $product, "ADDLOCAL=Desktop")) -eq 0) "Desktop is added"
    Assert-True (Test-Path -LiteralPath (Join-Path $Folder "domovoi-desktop.exe")) "domovoi-desktop.exe is installed"

    Write-Host "==> 5. The server-only installer beside the full one"
    Assert-True ((Invoke-Msiexec "5-server-beside" @("/i", "`"$server`"")) -ne 0) "Domovoi Server is refused"
    Assert-True ((Get-Installed $ServerCode).Count -eq 0) "Domovoi Server is not installed"

    Write-Host "==> 6. Uninstall"
    Assert-True ((Invoke-Msiexec "6-uninstall" @("/x", $product)) -eq 0) "Domovoi uninstalls"
    Assert-True ((Get-Installed $FullCode).Count -eq 0) "no Domovoi product is left"
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $Folder "domovoi-server.exe"))) "its programs are gone"
    Assert-True (-not (Test-RunValue)) "its Run value is gone"
    Assert-True (Test-Path -LiteralPath $marker) "the app's data stays (docs/windows.md)"

    Write-Host "==> 7. The full installer after the server-only one"
    Assert-True ((Invoke-Msiexec "7-server" @("/i", "`"$server`"", "ADDLOCAL=Server,ServerAtSignIn")) -eq 0) "Domovoi Server installs, starting at sign-in"
    Assert-True ((Invoke-Msiexec "7-full" @("/i", "`"$old`"")) -eq 0) "Domovoi installs after it"
    Assert-True ((Get-Installed $ServerCode).Count -eq 0) "Domovoi Server was replaced"
    Assert-True ((Get-InstalledVersion $FullCode) -eq $version) "Domovoi $version is installed"
    Assert-True (Test-RunValue) "Start at sign-in carried over"
    Write-Host "==> The installers passed every step"
} finally {
    Write-Host "==> Removing what the cycle installed"
    Remove-Everything
    Remove-Item -LiteralPath $marker -ErrorAction SilentlyContinue
    if (-not $hadData) { Remove-Item -LiteralPath $data -Recurse -ErrorAction SilentlyContinue }
}
