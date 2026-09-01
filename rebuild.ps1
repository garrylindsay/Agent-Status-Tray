#Requires -Version 5.1
<#
    .SYNOPSIS
        Rebuilds the tray and puts it back in the notification area.

    .DESCRIPTION
        Windows locks a running exe against writes, so `cargo build` cannot relink while the tray
        is running -- the tray has to be stopped first. Stopping it is forced on you; starting it
        again is not, which is how a rebuild quietly leaves you with no tray at all. This does both
        halves.

    .EXAMPLE
        .\rebuild.ps1
        .\rebuild.ps1 --features foo    # trailing arguments go to cargo
#>
[CmdletBinding()]
param(
    # Build only. Useful when you are about to launch it yourself, e.g. under a debugger.
    [switch] $NoStart,

    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $CargoArgs
)

$ErrorActionPreference = 'Stop'

$name = 'agent-status-tray'
$exe = Join-Path $PSScriptRoot "target\release\$name.exe"

$running = @(Get-Process -Name $name -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    Write-Host "Stopping $($running.Count) running $name (pid $($running.Id -join ', '))"
    $running | Stop-Process
    # The link step needs the file handle gone, not just the kill delivered. The single-instance
    # mutex is released on process exit too, so this same wait is what lets the relaunch succeed.
    $running | Wait-Process -Timeout 10
}

# cargo reports progress on stderr, and Windows PowerShell turns a native command's stderr into
# error records. Under 'Stop' that makes ordinary "Compiling ..." output a terminating error, but
# only when the caller redirects streams -- so it works from -File and blows up when dot-called.
# The exit code is the honest answer either way.
$outer = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
cargo build --release @CargoArgs
$code = $LASTEXITCODE
$ErrorActionPreference = $outer

if ($code -ne 0) {
    Write-Host "Build failed (exit $code)." -ForegroundColor Red
}

if (-not $NoStart) {
    if (Test-Path $exe) {
        # Started even when the build failed: the exe on disk is then the previous build, and an
        # older tray is worth more than no tray -- a failed build is the moment you least want to
        # lose sight of your sessions.
        Start-Process -FilePath $exe -WorkingDirectory $PSScriptRoot
        $what = if ($code -eq 0) { 'new build' } else { 'previous build' }
        Write-Host "Tray restarted ($what)." -ForegroundColor Green
    }
    else {
        Write-Host "No exe at $exe -- nothing to start." -ForegroundColor Yellow
    }
}

exit $code
