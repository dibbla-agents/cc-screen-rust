# ccs installer (Windows), served by the hub at GET /ccs.ps1 (proposal 0060 D3).
#
#   irm <hub>/ccs.ps1 | iex
#
# Wraps the cargo-dist PowerShell installer for the `ccs` terminal client and
# ends by PRINTING the sign-in command (the pipe has no interactive console).
$ErrorActionPreference = "Stop"

$HubUrl = "__CCSCREEN_HUB_URL__"
$InstallerUrl = "__CCSCREEN_INSTALLER_URL__"

Write-Host "-> installing ccs (the cc-screen terminal client)"
Invoke-Expression (Invoke-RestMethod $InstallerUrl)

Write-Host ""
Write-Host "ccs installed. Now sign this terminal in:"
Write-Host ""
Write-Host "  ccs activate --server $HubUrl"
Write-Host ""
Write-Host "(prints a one-time code; approve it from any logged-in browser - your phone works)"
