# Walks the WinUI app's screens and saves a picture of each (`just windows-snapshots`).
#
# A server of its own with an empty home is started on a spare port, the app
# pairs with it (keeping nothing: DOMOVOI_STORE=memory), and the screens are
# reached through UI Automation and captured with PrintWindow, so the mouse
# and keyboard are left alone and the window need not be in front. A control
# that is not found fails the run: a screen that lost its button is a
# regression too.
#
# The server runs from a copy: a running executable cannot be linked again,
# and a build may start while this runs.
param(
    [string]$Out = 'target/ui-snapshots/windows',
    [int]$Port = 13987,
    [string]$Build = 'target/debug'
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Capture {
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
'@
[void][Capture]::SetProcessDPIAware()

$repo = (Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path
$build = Join-Path $repo $Build
$out = Join-Path $repo $Out
$work = Join-Path ([System.IO.Path]::GetTempPath()) "domovoi-snapshots-$PID"
New-Item -ItemType Directory -Force $out, "$work/home", "$work/client" | Out-Null
$server = Join-Path $work 'domovoi-server.exe'
Copy-Item (Join-Path $build 'domovoi-server.exe') $server

$automation = [System.Windows.Automation.AutomationElement]
$scope = [System.Windows.Automation.TreeScope]::Descendants
# The control named `$name` that can do `$pattern`. A button's tooltip has the
# button's name too, so the name alone does not say which is meant.
function Find-Named([System.Windows.Automation.AutomationElement]$from, [string]$name, $pattern) {
    $condition = New-Object System.Windows.Automation.PropertyCondition($automation::NameProperty, $name)
    # A screen takes a moment to draw: look for a few seconds before giving up.
    for ($try = 0; $try -lt 20; $try++) {
        foreach ($found in $from.FindAll($scope, $condition)) {
            $does = $null
            if ($found.TryGetCurrentPattern($pattern, [ref]$does)) { return $does }
        }
        Start-Sleep -Milliseconds 300
    }
    throw "the screen has no control named '$name' to use"
}
function Invoke-Named([string]$name, $from = $script:window) {
    (Find-Named $from $name ([System.Windows.Automation.InvokePattern]::Pattern)).Invoke()
}
function Select-Named([string]$name) {
    (Find-Named $script:window $name ([System.Windows.Automation.SelectionItemPattern]::Pattern)).Select()
}
function Save-Screen([string]$name) {
    Start-Sleep -Milliseconds 1500
    $rect = New-Object Capture+RECT
    [void][Capture]::GetWindowRect($script:handle, [ref]$rect)
    $bitmap = New-Object System.Drawing.Bitmap(($rect.Right - $rect.Left), ($rect.Bottom - $rect.Top))
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $dc = $graphics.GetHdc()
    [void][Capture]::PrintWindow($script:handle, $dc, 2)
    $graphics.ReleaseHdc($dc); $graphics.Dispose()
    $path = Join-Path $out "$name.png"
    $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); $bitmap.Dispose()
    Write-Host "  $name.png"
}

$serverProcess = $null
$app = $null
try {
    $serverProcess = Start-Process $server -ArgumentList '--home-dir', "$work/home", '--port', $Port, '--no-startup-code', '--no-session-import' -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 4
    $pairing = & $server --home-dir "$work/home" --port $Port pair | Out-String
    if ($pairing -notmatch 'token=([A-Z0-9-]+)') { throw 'the server gave no pairing code' }

    $env:DOMOVOI_STORE = 'memory'
    $env:DOMOVOI_DATA = "$work/client"
    $env:DOMOVOI_NO_REGISTER = '1'
    $env:DOMOVOI_SERVER = "http://127.0.0.1:$Port"
    $env:DOMOVOI_PAIRING = $Matches[1]
    $app = Start-Process (Join-Path $build 'Domovoi.exe') -PassThru
    for ($try = 0; $try -lt 40 -and $app.MainWindowHandle -eq 0; $try++) { Start-Sleep -Milliseconds 500; $app.Refresh() }
    if ($app.MainWindowHandle -eq 0) { throw 'the app opened no window' }
    $script:handle = $app.MainWindowHandle
    $script:window = $automation::FromHandle($script:handle)
    Write-Host "Pictures in $out"

    # Connected, with no project yet.
    Invoke-Named 'New thread'
    Save-Screen '01-new-thread-empty'

    # This repository as a project: the picker with a tile, its icon and the workspace choice.
    (Find-Named $script:window 'Add project' ([System.Windows.Automation.ExpandCollapsePattern]::Pattern)).Expand()
    Invoke-Named 'Add folder by path…' $automation::RootElement
    (Find-Named $script:window 'Server folder' ([System.Windows.Automation.ValuePattern]::Pattern)).SetValue($repo)
    Save-Screen '02-add-folder'
    Invoke-Named 'Add Project'
    Start-Sleep -Seconds 2
    Invoke-Named 'New thread'
    Save-Screen '03-new-thread'

    Invoke-Named 'More'
    Invoke-Named 'Group Threads by Project' $automation::RootElement
    Save-Screen '04-by-project'
    Invoke-Named 'More'
    Invoke-Named 'Group Threads by Status' $automation::RootElement
    # A menu that is still closing does not open again.
    Start-Sleep -Milliseconds 800

    # The palette, and from it the panel beside the thread with each of its tabs.
    Invoke-Named 'More'
    Invoke-Named 'Command Palette…  (Ctrl+K)' $automation::RootElement
    Save-Screen '04-palette'
    Invoke-Named 'Toggle side panel'
    foreach ($tab in 'Diff', 'Artifacts', 'Terminal', 'Files', 'Git') {
        Invoke-Named "$tab panel"
        Save-Screen ("04-panel-" + $tab.ToLower())
    }
    Invoke-Named 'Close the panel'

    Invoke-Named 'Settings'
    foreach ($section in 'General', 'Appearance', 'Servers', 'Companion', 'Projects', 'Providers', 'Scheduled Tasks', 'Keybindings', 'Access & Remote', 'Archived', 'Maintenance') {
        Select-Named $section
        Save-Screen ("05-settings-" + ($section.ToLower() -replace '[^a-z]+', '-'))
    }
    Write-Host 'Every screen was reached.'
}
finally {
    if ($app -and -not $app.HasExited) { $app.Kill() }
    if ($serverProcess) {
        & $server --home-dir "$work/home" --port $Port stop | Out-Null
        if (-not $serverProcess.WaitForExit(5000)) { $serverProcess.Kill() }
    }
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
