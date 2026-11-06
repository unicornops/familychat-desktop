# link-handler-smoke.ps1 - Checks that the installed Family Chat receives a
# familychat: link opened through Windows (ADR 0014), as a browser would
# hand it over: the MSI's HKCU\Software\Classes\familychat handler starts
# familychat-launcher.exe "%1", which starts familychat.exe with the link
# and no console, which hands it to the running window over the
# single-instance pipe (ADR 0013). scripts/link-handler-smoke.sh does the
# same on Linux and macOS.
#
# Usage: ./scripts/link-handler-smoke.ps1 -App "$env:LOCALAPPDATA\Programs\Family Chat\familychat.exe" -OutputDir artifacts/link-handler
#
# It starts the installed familychat.exe (the window) on a throwaway
# profile, waits until it listens for second launches, opens a sign-in link
# with a placeholder code through the shell (Start-Process, ShellExecute),
# and checks the window's log says a second launch was served and
# delivered, that the second launch exited and the window still runs, and
# that no log line or file holds the code. The screenshot
# (OutputDir\link.png) shows the "Sign in as ...?" confirmation; nothing is
# redeemed without Continue, so no request is sent.

param(
    [Parameter(Mandatory = $true)][string]$App,
    [Parameter(Mandatory = $true)][string]$OutputDir,
    [int]$WaitSeconds = 90
)

$ErrorActionPreference = 'Stop'
$PlaceholderCode = 'fc-link-smoke-placeholder-code'
$Link = "familychat://safechat.family/app/login?account_provider=smith.safechat.family&login_hint=mxid:%40alex%3Asmith.safechat.family&hs=smith.safechat.family&token=$PlaceholderCode"

if (-not (Test-Path $App)) {
    Write-Error "no installed app at $App"
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

# A throwaway profile; the shell's handler inherits this environment, so
# the second launch finds the same profile and its pipe.
$DataDir = Join-Path ([System.IO.Path]::GetTempPath()) ("familychat-link-smoke-" + [System.Guid]::NewGuid())
New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
$env:FAMILYCHAT_DATA_DIR = $DataDir
$env:FAMILYCHAT_LOG = 'warn,familychat=info,familychat_instance=debug'
$ProfileLog = Join-Path $DataDir 'logs\familychat.log'

$stdout = Join-Path $OutputDir 'app.out.log'
$stderr = Join-Path $OutputDir 'app.log'
$process = Start-Process -FilePath $App -PassThru -WindowStyle Normal `
    -RedirectStandardOutput $stdout -RedirectStandardError $stderr

function Show-Logs {
    Write-Host '--- app output'
    Get-Content $stdout, $stderr -ErrorAction SilentlyContinue
    Write-Host '--- profile log'
    Get-Content $ProfileLog -ErrorAction SilentlyContinue
}

# Waits until a line of the profile's log holds every one of $Texts.
function Wait-ForLog([string[]]$Texts) {
    for ($i = 0; $i -lt $WaitSeconds; $i++) {
        $process.Refresh()
        if ($process.HasExited) {
            Show-Logs
            Write-Error "the app exited (code $($process.ExitCode)) waiting for: $($Texts -join ', ')"
        }
        if (Test-Path $ProfileLog) {
            $lines = Get-Content $ProfileLog -ErrorAction SilentlyContinue
            foreach ($text in $Texts) {
                $lines = @($lines | Where-Object { $_.Contains($text) })
            }
            if ($lines.Count -gt 0) {
                return
            }
        }
        Start-Sleep -Seconds 1
    }
    Show-Logs
    Write-Error "no log line with: $($Texts -join ', ')"
}

try {
    Wait-ForLog @('listening for second launches')
    # Let the profile open, so the link lands on the sign-in screen.
    Start-Sleep -Seconds 5
    $command = (Get-ItemProperty 'HKCU:\Software\Classes\familychat\shell\open\command').'(default)'
    Write-Host "The window is listening (pid $($process.Id)); familychat: runs $command"

    Start-Process $Link
    Wait-ForLog @('served a second launch', 'Delivered')
    Write-Host 'Windows started the handler with the link, and it was handed to the window'

    # The launcher and the second familychat.exe exit once the link is
    # handed over; only the window stays.
    Start-Sleep -Seconds 5
    $process.Refresh()
    if ($process.HasExited) {
        Show-Logs
        Write-Error "the window exited (code $($process.ExitCode)) after the link"
    }
    $running = @(Get-Process -Name familychat, familychat-launcher -ErrorAction SilentlyContinue)
    if ($running.Count -ne 1) {
        $running | Format-Table Id, ProcessName, Path | Out-String | Write-Host
        Show-Logs
        Write-Error "$($running.Count) Family Chat processes run, expected only the window"
    }

    $screenshot = Join-Path $OutputDir 'link.png'
    Save-Screen $screenshot
    Write-Host "Screenshot: $screenshot"
} finally {
    if (-not $process.HasExited) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        $process.WaitForExit(5000) | Out-Null
    }
    if (Test-Path $ProfileLog) {
        Copy-Item $ProfileLog (Join-Path $OutputDir 'profile.log')
    }
}

$leaks = @(Get-ChildItem -Recurse -File $DataDir, $OutputDir |
    Where-Object { $_.Extension -ne '.png' } |
    Select-String -SimpleMatch -Pattern $PlaceholderCode -List)
if ($leaks.Count -gt 0) {
    $leaks | ForEach-Object { Write-Host $_.Path }
    Write-Error "the link's code reached a log or a file"
}
Write-Host 'No log line or file holds the code'
Remove-Item -Recurse -Force $DataDir -ErrorAction SilentlyContinue
