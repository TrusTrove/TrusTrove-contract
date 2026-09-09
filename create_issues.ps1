# Thin wrapper around scripts/maintainer/create-contract-issues.ps1
# Delegates to the canonical maintainer script which handles rate-limit
# retries and duplicate-issue guards.

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$MaintainerScript = Join-Path $ScriptDir "scripts\maintainer\create-contract-issues.ps1"

& $MaintainerScript @args
