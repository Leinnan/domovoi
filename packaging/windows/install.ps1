# Installs or uninstalls a built MSI for this user, with a verbose log in
# target\ (`just windows-install`, `just windows-uninstall`).
[CmdletBinding()]
param(
    [ValidateSet("install", "uninstall")]
    [string]$Action = "install",
    [ValidateSet("full", "server")]
    [string]$Flavor = "full"
)

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "common.ps1")
$version = Get-DomovoiVersion
$name = if ($Flavor -eq "server") { "Domovoi-Server-$version-windows-x64.msi" } else { "Domovoi-$version-windows-x64.msi" }
$msi = Join-Path $DomovoiDist $name
if (-not (Test-Path -LiteralPath $msi)) { throw "No $name in ${DomovoiDist}: build it with ``just windows-installers``" }
$log = Join-Path $DomovoiRepo "target\windows-$Action.log"
$switch = if ($Action -eq "install") { "/i" } else { "/x" }
$run = Start-Process msiexec.exe -Wait -PassThru -ArgumentList @($switch, "`"$msi`"", "/passive", "/l*v", "`"$log`"")
# 3010: done, and Windows asks for a restart.
if ($run.ExitCode -notin @(0, 3010)) { throw "msiexec $switch $name failed with exit code $($run.ExitCode) (log: $log)" }
Write-Host "$Action of $name done (log: $log)"
