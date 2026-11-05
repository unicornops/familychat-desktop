# window-smoke.ps1 - Opens the Family Chat window on Windows and checks it
# appears (scripts/window-smoke.sh does the same on Linux and macOS).
#
# Usage: ./scripts/window-smoke.ps1 -App target/release/familychat.exe -OutputDir artifacts/window-smoke
#
# Starts the app with no arguments (the GUI), waits for its main window to
# be titled "Family Chat", checks the process is still running a few
# seconds later, saves a screenshot and the app's output to OutputDir, and
# quits it.

param(
    [Parameter(Mandatory = $true)][string]$App,
    [Parameter(Mandatory = $true)][string]$OutputDir,
    [int]$WaitSeconds = 60
)

$ErrorActionPreference = 'Stop'
$Title = 'Family Chat'

if (-not (Test-Path $App)) {
    Write-Error "no app binary at $App; build it first"
}
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$OutputDir = (Resolve-Path $OutputDir).Path

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

function Save-Screen([string]$Path) {
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bitmap = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

$stdout = Join-Path $OutputDir 'app.out.log'
$stderr = Join-Path $OutputDir 'app.log'
$process = Start-Process -FilePath $App -PassThru -WindowStyle Normal `
    -RedirectStandardOutput $stdout -RedirectStandardError $stderr

try {
    $found = $false
    for ($i = 0; $i -lt $WaitSeconds; $i++) {
        $process.Refresh()
        if ($process.HasExited) {
            Get-Content $stderr -ErrorAction SilentlyContinue
            Write-Error "the app exited (code $($process.ExitCode)) before its window appeared"
        }
        if ($process.MainWindowTitle -eq $Title) {
            $found = $true
            break
        }
        Start-Sleep -Seconds 1
    }
    if (-not $found) {
        Get-Content $stderr -ErrorAction SilentlyContinue
        Write-Error "no '$Title' window appeared within $WaitSeconds seconds (main window title: '$($process.MainWindowTitle)')"
    }
    Write-Host "The '$Title' window is open (handle $($process.MainWindowHandle))."

    # Let it draw a few frames, then check it is still running.
    Start-Sleep -Seconds 3
    $process.Refresh()
    if ($process.HasExited) {
        Get-Content $stderr -ErrorAction SilentlyContinue
        Write-Error "the app exited (code $($process.ExitCode)) after opening its window"
    }
    $screenshot = Join-Path $OutputDir 'window.png'
    Save-Screen $screenshot
    Write-Host "Screenshot: $screenshot"
} finally {
    if (-not $process.HasExited) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        $process.WaitForExit(5000) | Out-Null
    }
    Write-Host 'App output:'
    Get-Content $stdout, $stderr -ErrorAction SilentlyContinue
}
