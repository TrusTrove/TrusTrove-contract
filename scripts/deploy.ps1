#!/usr/bin/env pwsh
# deploy.ps1 — Idempotent deployment script for TrusTrove contracts (PowerShell)
#
# issue #827: every contract is deployed with its protocol `__constructor`
# arguments, so a successful deploy is also a successful initialization —
# there is no separate `initialize` transaction anymore.  escrow and pool
# each receive the OTHER's address in their constructor; the pool address is
# pre-computed from a persisted deploy salt (the stellar CLI derives a
# constructor deploy's id from deployer+salt only) so the pair can be
# deployed in two atomic one-step transactions.
#
# Usage:
#   powershell ./scripts/deploy.ps1              # Normal deploy (skips already-deployed contracts)
#   powershell ./scripts/deploy.ps1 -Fresh        # Ignore saved addresses and redeploy everything
#   powershell ./scripts/deploy.ps1 -DryRun       # Show what would be deployed without actually deploying
#   powershell ./scripts/deploy.ps1 -Help         # Show this help
#
# Deployed addresses are persisted to .deployed-addresses after each successful
# deployment step.  Re-running the script after a partial failure will skip any
# step whose address was already saved.  The pool deployment salts
# (pool_usdc_salt / pool_xlm_salt) are persisted BEFORE the escrow deploy so a
# resumed run reconstructs the identical pool-address prediction.

$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------------------
# 0. CLI / env setup
# ---------------------------------------------------------------------------

$fresh = $false
$resume = $false
$dryRun = $false

foreach ($arg in $args) {
    switch -Wildcard ($arg) {
        '-fresh'    { $fresh = $true }
        '-resume'   { $resume = $true }
        '-dryrun'   { $dryRun = $true }
        '-help'     { Write-Host "deploy.ps1 — Idempotent deployment script for TrusTrove contracts"; exit 0 }
        '-h'        { Write-Host "deploy.ps1 — Idempotent deployment script for TrusTrove contracts"; exit 0 }
        default     { Write-Host "Unknown argument: $arg  (use -Help for usage)"; exit 1 }
    }
}

# Resolve stellar CLI location dynamically
$stellar = Get-Command stellar -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
if (-not $stellar) {
    $stellarBin = [Environment]::GetEnvironmentVariable('STELLAR_BIN')
    if ($stellarBin -and (Test-Path $stellarBin)) {
        $stellar = $stellarBin
    }
    elseif (Test-Path "${env:ProgramFiles(x86)}\Stellar CLI\stellar.exe") {
        $stellar = "${env:ProgramFiles(x86)}\Stellar CLI\stellar.exe"
    }
    else {
        Write-Host "Error: stellar CLI not found."
        Write-Host ""
        Write-Host "Try one of:"
        Write-Host "  1. Install stellar CLI globally (https://developers.stellar.org/docs/learn/developing-with-soroban/setup)"
        Write-Host "  2. Set STELLAR_BIN=/path/to/stellar.exe"
        Write-Host "  3. Ensure 'Stellar CLI' is installed in Program Files (x86)"
        Write-Host ""
        exit 1
    }
}

# Load .env file
$envFile = if (Test-Path .env) { ".env" } else { ".env.example" }
Get-Content $envFile | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') {
        [Environment]::SetEnvironmentVariable($matches[1], $matches[2])
    }
}

# ---------------------------------------------------------------------------
# 1. Address persistence helpers
# ---------------------------------------------------------------------------

$addressesFile = ".deployed-addresses"

if ($fresh) {
    Write-Host "=== -Fresh flag set: removing saved addresses and starting clean ==="
    Remove-Item -Path $addressesFile -ErrorAction SilentlyContinue
}

# Ensure file exists
if (-not (Test-Path $addressesFile)) {
    New-Item -Path $addressesFile -ItemType File -Force | Out-Null
}

# Read all saved addresses into a hashtable
function Get-SavedAddresses {
    $map = @{}
    if (Test-Path $addressesFile) {
        Get-Content $addressesFile | ForEach-Object {
            if ($_ -match '^([^=]+)=(.*)$') {
                $map[$matches[1]] = $matches[2]
            }
        }
    }
    return $map
}

function Save-Address {
    param($key, $value)
    $map = Get-SavedAddresses
    $map[$key] = $value
    $map.Keys | ForEach-Object { "$_=$($map[$_])" } | Set-Content $addressesFile
}

function Load-Address {
    param($key)
    $map = Get-SavedAddresses
    return $map[$key]
}

# ---------------------------------------------------------------------------
# 2. Transaction confirmation polling
# ---------------------------------------------------------------------------

function Wait-ForContract {
    param($contractId)
    $maxAttempts = 15
    $attempt = 0
    $delay = 2

    Write-Host "  Waiting for contract $contractId to be confirmed on-chain..."
    while ($attempt -lt $maxAttempts) {
        $output = & $stellar contract fetch --id $contractId --network testnet --output xdr 2>&1
        if ($LASTEXITCODE -eq 0) {
            Write-Host "  Confirmed."
            return $true
        }
        $attempt++
        Write-Host "  Attempt $attempt/$maxAttempts — not yet visible, retrying in ${delay}s..."
        Start-Sleep -Seconds $delay
    }

    Write-Host "ERROR: Contract $contractId was not confirmed after $(($maxAttempts * $delay))s."
    Write-Host "       Check your network connection or the Stellar testnet status."
    Write-Host "       The address has NOT been saved.  Re-run the script to resume."
    return $false
}

# ---------------------------------------------------------------------------
# 3. Core deployment helper
# ---------------------------------------------------------------------------

# Deploy a contract and run its protocol `__constructor` in the SAME
# transaction (issue #827: deploy and initialization are atomic).
#   $key       — persistence key in .deployed-addresses
#   $wasm      — wasm path
#   $salt      — optional 32-byte hex salt fixing the deploy address, so the
#                address can be predicted up-front (see Predict-ContractId)
#   $ctorArgs  — name/value flag pairs passed verbatim after `--`, e.g.
#                @('--admin', $deployerAddress)
function Deploy-Contract {
    param($key, $wasm, $salt = "", [string[]]$ctorArgs = @())

    $existing = Load-Address $key
    if ($existing) {
        Write-Host "  $key already deployed at $existing — skipping."
        return $existing
    }

    if ($dryRun) {
        Write-Host "  [DRY-RUN] Would deploy $key from $wasm"
        if ($salt) { Write-Host "  [DRY-RUN]   salt: $salt" }
        Write-Host "  [DRY-RUN]   __constructor args: $($ctorArgs -join ' ')"
        return "<dry-run>"
    }

    if (-not (Test-Path $wasm)) {
        Write-Host "ERROR: WASM file not found: $wasm"
        Write-Host "       Run 'stellar contract build' first, or check the build output."
        throw "WASM not found: $wasm"
    }

    Write-Host "  Deploying $key from $wasm ..."
    $deployArgs = @('contract', 'deploy', '--wasm', $wasm,
                    '--source', $env:DEPLOYER_ACCOUNT, '--network', 'testnet')
    if ($salt) { $deployArgs += @('--salt', $salt) }
    if ($ctorArgs.Count -gt 0) {
        # Positional args after `--` are forwarded to __constructor.
        $deployArgs += '--'
        $deployArgs += $ctorArgs
    }
    $output = & $stellar @deployArgs 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "ERROR: Deploy failed for $key:"
        Write-Host "  $output"
        Write-Host "  Fix the error above and rerun — already-deployed contracts will be skipped."
        throw "Deploy failed for $key"
    }

    # Extract the contract id (the CLI merges INFO noise into the stream).
    $match = [regex]::Matches(($output | Out-String), 'C[A-Z2-7]{55}')
    if ($match.Count -eq 0) {
        Write-Host "ERROR: Deploy for $key succeeded but no contract id was found in output:"
        Write-Host "  $output"
        throw "Could not parse contract id for $key"
    }
    $contractId = $match[$match.Count - 1].Value

    if (-not (Wait-ForContract $contractId)) {
        throw "Contract $contractId was not confirmed"
    }

    Save-Address $key $contractId
    Write-Host "  Saved: $key=$contractId"
    return $contractId
}

# ---------------------------------------------------------------------------
# 3b. Address prediction for constructor-linked deploys (issue #827)
# ---------------------------------------------------------------------------

# Returns the persisted deploy salt for $key (stored as ${key}_salt),
# generating and persisting a fresh 32-byte hex salt on first use.  The
# salt is stored BEFORE the escrow/pool pair deploys so a resumed run
# reconstructs the identical pool-address prediction.
function Get-OrCreateSalt {
    param($key)
    $salt = Load-Address "${key}_salt"
    if (-not $salt) {
        $salt = -join ((1..32) | ForEach-Object { '{0:x2}' -f (Get-Random -Maximum 256) })
        Save-Address "${key}_salt" $salt
    }
    return $salt
}

# Computes the contract id that Deploy-Contract with the same salt will
# produce.  The stellar CLI derives constructor-deploy ids from
# (deployer, salt) only — constructor args are NOT part of the preimage —
# so this prediction is exact.
function Predict-ContractId {
    param($salt, $deployerAddress)
    $output = & $stellar contract id wasm --salt $salt `
        --source-account $deployerAddress --network testnet 2>&1
    $match = [regex]::Match(($output | Out-String), 'C[A-Z2-7]{55}')
    if (-not $match.Success) {
        Write-Host "ERROR: could not predict contract id for salt $salt"
        Write-Host "  $output"
        throw "Address prediction failed for salt $salt"
    }
    return $match.Value
}

# ---------------------------------------------------------------------------
# 4. Post-deploy wiring helper
# ---------------------------------------------------------------------------

# Runs `stellar contract invoke` for a post-deploy wiring call (invoice
# set_pool_contract / set_escrow_contract / add_supported_asset / ...).
# Since issue #827 the constructor replaces initialize, so this helper is no
# longer used for initialization — only for the optional wiring transactions,
# with the ${label}_initialized marker retained for resume compatibility.
function Invoke-Init {
    param($label, $contractId, [string[]]$initArgs)

    $initKey = "${label}_initialized"
    $existing = Load-Address $initKey
    if ($existing) {
        Write-Host "  $label already applied — skipping."
        return
    }

    if ($dryRun) {
        Write-Host "  [DRY-RUN] Would invoke on $label ($contractId) with: $($initArgs -join ' ')"
        return
    }

    Write-Host "  Invoking $label ($contractId) ..."
    $output = & $stellar contract invoke --id $contractId --source $env:DEPLOYER_ACCOUNT --network testnet @initArgs 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "ERROR: Invocation failed for $label:"
        Write-Host "  $output"
        Write-Host "  The contract is deployed but this wiring step did not complete."
        Write-Host "  Fix the error and rerun — this step will be retried automatically."
        throw "Invocation failed for $label"
    }

    Save-Address $initKey "true"
    Write-Host "  $label applied successfully."
}

# ---------------------------------------------------------------------------
# 5. Pre-flight checks
# ---------------------------------------------------------------------------

Write-Host "`n=== Pre-flight checks ==="

$deployerAccount = [Environment]::GetEnvironmentVariable('DEPLOYER_ACCOUNT')
if (-not $deployerAccount) {
    Write-Host "ERROR: DEPLOYER_ACCOUNT is not set.  Check your .env file."
    exit 1
}

$usdcIssuer = [Environment]::GetEnvironmentVariable('USDC_ISSUER')
if (-not $usdcIssuer) {
    Write-Host "ERROR: USDC_ISSUER is not set.  Check your .env file."
    exit 1
}

$xlmAsset = [Environment]::GetEnvironmentVariable('XLM_ASSET')
if (-not $xlmAsset) {
    Write-Host "ERROR: XLM_ASSET is not set.  Check your .env file."
    exit 1
}

$deployerAddress = & $stellar keys address $deployerAccount 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Host "ERROR: Could not resolve address for key '$deployerAccount'."
    Write-Host "       Run scripts/setup-testnet.ps1 first."
    exit 1
}
Write-Host "  Deployer address : $deployerAddress"
Write-Host "  Addresses file   : $addressesFile"
Write-Host "  Fresh deploy     : $fresh"
Write-Host "  Dry-run mode     : $dryRun"
Write-Host ""

# ---------------------------------------------------------------------------
# 6. Build
# ---------------------------------------------------------------------------

if ($dryRun) {
    Write-Host "=== [DRY-RUN] Skipping build ==="
} else {
    Write-Host "=== Building all contracts ==="
    & $stellar contract build
}
Write-Host ""

# ---------------------------------------------------------------------------
# 7. Deploy all contracts (each deploy atomically runs its __constructor)
# ---------------------------------------------------------------------------

Write-Host "=== Deploying registry_contract (constructor runs inline) ==="
$registryId = Deploy-Contract "registry" "target/wasm32v1-none/release/trusttrove_registry.wasm" "" @('--admin', $deployerAddress)
Write-Host "Registry: $registryId"

Write-Host "`n=== Deploying invoice_contract (constructor runs inline) ==="
if (-not $registryId -and -not $dryRun) {
    Write-Host "ERROR: invoice's constructor needs the registry address,"
    Write-Host "       but no registry deployment is recorded. Deploy registry first."
    exit 1
}
$invoiceId = Deploy-Contract "invoice" "target/wasm32v1-none/release/trusttrove_invoice.wasm" "" @(
    '--admin', $deployerAddress,
    '--registry_contract', $registryId
)
Write-Host "Invoice: $invoiceId"

# ---------------------------------------------------------------------------
# 7b. USDC escrow/pool pair — constructor-linked via a pre-computed pool id
#
# escrow's constructor receives the pool address and pool's constructor
# receives the escrow address.  The deploy address is derived from
# (deployer, salt) only, so we persist a salt for the pool, predict its
# future address, construct escrow against that prediction, then deploy the
# pool AT that same salt/address.
# ---------------------------------------------------------------------------

Write-Host "`n=== Deploying USDC escrow_contract (constructed against predicted pool id) ==="
$poolUsdcSalt = Get-OrCreateSalt "pool_usdc"
if ($dryRun) {
    $poolUsdcTargetId = "<predicted-pool-usdc-id>"
} else {
    $poolUsdcTargetId = Load-Address "pool_usdc"
    if (-not $poolUsdcTargetId) {
        $poolUsdcTargetId = Predict-ContractId $poolUsdcSalt $deployerAddress
    }
}
$escrowUsdcId = Deploy-Contract "escrow_usdc" "target/wasm32v1-none/release/trusttrove_escrow.wasm" "" @(
    '--admin', $deployerAddress,
    '--pool_contract', $poolUsdcTargetId,
    '--usdc_asset', $usdcIssuer
)
Write-Host "USDC Escrow: $escrowUsdcId"

Write-Host "`n=== Deploying USDC pool_contract (single step, no separate init) ==="
$poolUsdcId = Deploy-Contract "pool_usdc" "target/wasm32v1-none/release/trusttrove_pool.wasm" $poolUsdcSalt @(
    '--admin', $deployerAddress,
    '--invoice_contract', $invoiceId,
    '--escrow_contract', $escrowUsdcId,
    '--funding_asset', $usdcIssuer,
    '--registry_contract', $registryId,
    '--treasury', $deployerAddress,
    '--min_initial_deposit', "10000000",
    '--share_name', "`"TrusTrove USDC Pool Shares`"",
    '--share_symbol', "`"TT-USDC`"",
    '--share_decimals', "7"
)
# The pool MUST land on the address escrow was constructed against.
if (-not $dryRun -and $poolUsdcId -ne $poolUsdcTargetId) {
    Write-Host "ERROR: pool_usdc deployed at $poolUsdcId but escrow_usdc was"
    Write-Host "       constructed against $poolUsdcTargetId. Aborting before"
    Write-Host "       saving inconsistent addresses. Investigate pool_usdc_salt in"
    Write-Host "       $addressesFile and redeploy the pair."
    exit 1
}
Write-Host "USDC Pool: $poolUsdcId"

Write-Host "`n=== Deploying XLM escrow_contract (EXPERIMENTAL, constructed against predicted pool id) ==="
$poolXlmSalt = Get-OrCreateSalt "pool_xlm"
if ($dryRun) {
    $poolXlmTargetId = "<predicted-pool-xlm-id>"
} else {
    $poolXlmTargetId = Load-Address "pool_xlm"
    if (-not $poolXlmTargetId) {
        $poolXlmTargetId = Predict-ContractId $poolXlmSalt $deployerAddress
    }
}
$escrowXlmId = Deploy-Contract "escrow_xlm" "target/wasm32v1-none/release/trusttrove_escrow.wasm" "" @(
    '--admin', $deployerAddress,
    '--pool_contract', $poolXlmTargetId,
    '--usdc_asset', $xlmAsset
)
Write-Host "XLM Escrow: $escrowXlmId"

Write-Host "`n=== Deploying XLM pool_contract (EXPERIMENTAL, single step, no separate init) ==="
$poolXlmId = Deploy-Contract "pool_xlm" "target/wasm32v1-none/release/trusttrove_pool.wasm" $poolXlmSalt @(
    '--admin', $deployerAddress,
    '--invoice_contract', $invoiceId,
    '--escrow_contract', $escrowXlmId,
    '--funding_asset', $xlmAsset,
    '--registry_contract', $registryId,
    '--treasury', $deployerAddress,
    '--min_initial_deposit', "10000000",
    '--share_name', "`"TrusTrove XLM Pool Shares`"",
    '--share_symbol', "`"TT-XLM`"",
    '--share_decimals', "7"
)
if (-not $dryRun -and $poolXlmId -ne $poolXlmTargetId) {
    Write-Host "ERROR: pool_xlm deployed at $poolXlmId but escrow_xlm was"
    Write-Host "       constructed against $poolXlmTargetId. Aborting before"
    Write-Host "       saving inconsistent addresses. Investigate pool_xlm_salt in"
    Write-Host "       $addressesFile and redeploy the pair."
    exit 1
}
Write-Host "XLM Pool: $poolXlmId"

Write-Host "`n=== Wiring USDC pool_contract into invoice_contract ==="
Invoke-Init "invoice_set_pool" $invoiceId @("--", "set_pool_contract", "--pool_contract", $poolUsdcId)

# Without the escrow wiring below, repay / repay_partial / repay_early panic
# with InvoiceError::NotFound when reading DataKey::EscrowContract.
Write-Host "`n=== Wiring USDC escrow_contract into invoice_contract ==="
Invoke-Init "invoice_set_escrow" $invoiceId @("--", "set_escrow_contract", "--escrow_contract", $escrowUsdcId)

# Without allow-listing, create rejects every invoice with UnsupportedAsset.
# add_supported_asset is itself idempotent on-chain (a no-op for an asset
# that is already allow-listed).
Write-Host "`n=== Allow-listing USDC as a supported funding asset ==="
Invoke-Init "invoice_add_asset_usdc" $invoiceId @("--", "add_supported_asset", "--asset", $usdcIssuer)

Write-Host "`n=== Allow-listing XLM as a supported funding asset (EXPERIMENTAL) ==="
Invoke-Init "invoice_add_asset_xlm" $invoiceId @("--", "add_supported_asset", "--asset", $xlmAsset)

# Agent-registry wiring is optional: it is only performed when
# AGENT_REGISTRY_CONTRACT is set in .env / .env.example.  Without it,
# submit_attestation panics with InvoiceError::NotFound and
# list_for_financing can never unlock.
$agentRegistryContract = [Environment]::GetEnvironmentVariable('AGENT_REGISTRY_CONTRACT')
if ($agentRegistryContract) {
    Write-Host "`n=== Wiring agent_registry_contract into invoice_contract ==="
    Invoke-Init "invoice_set_agent_registry" $invoiceId @("--", "set_agent_registry_contract", "--agent_registry_contract", $agentRegistryContract)
} else {
    Write-Host "`n=== AGENT_REGISTRY_CONTRACT is not set — skipping agent-registry wiring ==="
    Write-Host "    submit_attestation will fail until it is wired (see DEPLOYMENT.md)."
}

# ---------------------------------------------------------------------------
# 8. Persist final addresses
# ---------------------------------------------------------------------------

if ($dryRun) {
    Write-Host "=== [DRY-RUN] Skipping .env.deployed generation ==="
} else {
$envOut = ".env.deployed"
$timestamp = (Get-Date -Format 'yyyy-MM-ddTHH:mm:ssZ')
@"
# Generated by deploy.ps1 on $timestamp
# Copy these values into trusttrove-app .env.local

NEXT_PUBLIC_REGISTRY_CONTRACT_ID=$registryId
NEXT_PUBLIC_INVOICE_CONTRACT_ID=$invoiceId
NEXT_PUBLIC_ESCROW_USDC_CONTRACT_ID=$escrowUsdcId
NEXT_PUBLIC_ESCROW_XLM_CONTRACT_ID=$escrowXlmId # EXPERIMENTAL
NEXT_PUBLIC_POOL_USDC_CONTRACT_ID=$poolUsdcId
NEXT_PUBLIC_POOL_XLM_CONTRACT_ID=$poolXlmId # EXPERIMENTAL
"@ | Set-Content $envOut

Write-Host "`n==========================================="
Write-Host "Deployment complete."
Write-Host ""
Write-Host "Addresses saved to: $addressesFile"
Write-Host "Frontend env saved to: $envOut"
Write-Host ""
Write-Host "Add to trusttrove-app .env.local:"
Write-Host ""
Get-Content $envOut | ForEach-Object { Write-Host $_ }
Write-Host "==========================================="
}
