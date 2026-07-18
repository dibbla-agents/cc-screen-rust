#Requires -Version 5
# cc-screen-rust — install this machine and connect it to a cc-screen hub (Windows).
#
# This script is SERVED BY THE HUB at /install.ps1 with the hub URL baked in, so
# the only thing you supply is a name for this machine:
#
#     irm <hub>/install.ps1 | iex                     # names the box $env:COMPUTERNAME
#     & ([scriptblock]::Create((irm <hub>/install.ps1))) my-windows-box   # explicit name
#
# (The hub can also pre-bake the name via /install.ps1?name=<name>.)
#
# It (1) installs the cc-screen-rust.exe binary, (2) enrolls this machine with the
# hub — a short code appears that you approve from the dashboard — and (3) registers
# it as a Task Scheduler task that reconnects at logon. Re-running is safe.
$ErrorActionPreference = 'Stop'

$HubUrl = '__CCSCREEN_HUB_URL__'
$InstallerUrl = '__CCSCREEN_INSTALLER_URL__'
# Machine name: explicit arg wins, then a name baked in by the hub (?name=), else
# this host's name.
$Baked = '__CCSCREEN_MACHINE_NAME__'
$Machine = if ($args.Count -ge 1 -and $args[0]) { $args[0] }
           elseif ($Baked) { $Baked }
           else { $env:COMPUTERNAME }

Write-Host "==> Installing the cc-screen-rust binary..."
# Re-running this one-liner is also the UPDATE path, so a previous install may have
# the agent already running — and Windows locks a running .exe, so the installer
# can't overwrite it ("being used by another process"). Stop the task instance and
# any lingering process first; all best-effort (fine if nothing's there on a first
# install). The `install ... --enroll` step below re-registers the task and starts
# it again, so the agent comes right back on the new binary.
try { schtasks /End /TN cc-screen-rust 2>$null | Out-Null } catch {}
Get-Process cc-screen-rust -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 400   # let Windows release the file handle before the overwrite
# The cargo-dist PowerShell installer drops the binary into ~\.local\bin and adds
# it to the user PATH (matches install-path in dist-workspace.toml).
Invoke-RestMethod $InstallerUrl | Invoke-Expression

# Resolve the binary. cargo-dist installs to ~\.local\bin; the user PATH change
# isn't visible in THIS session yet, so look there first, then fall back to PATH.
$Bin = Join-Path $env:USERPROFILE '.local\bin\cc-screen-rust.exe'
if (-not (Test-Path $Bin)) {
    $cmd = Get-Command cc-screen-rust -ErrorAction SilentlyContinue
    if ($cmd) { $Bin = $cmd.Source }
}

Write-Host ""
Write-Host "==> Connecting '$Machine' to $HubUrl"
Write-Host "    A code will print below — approve it at $HubUrl/activate (you must be logged in)."
Write-Host ""
# One command: device flow (prints a code, waits for dashboard approval, saves the
# token), then registers the background service (--hub-only = reachable only via
# the hub, binds no local port). Reconnects at logon.
& $Bin install --hub $HubUrl --machine-id $Machine --hub-only --enroll
if ($LASTEXITCODE -ne 0) {
    Write-Error "install failed (exit $LASTEXITCODE): '$Machine' enrolled but the reconnect task was not registered."
    exit $LASTEXITCODE
}

Write-Host ""
Write-Host "OK — '$Machine' is connected and will reconnect automatically."
