#!/bin/bash
# deploy.sh — Idempotent deployment script for TrusTrove contracts
#
# Usage:
#   ./scripts/deploy.sh              # Normal deploy (skips already-deployed contracts)
#   ./scripts/deploy.sh --resume     # Explicit resume mode (same as default)
#   ./scripts/deploy.sh --fresh      # Ignore saved addresses and redeploy everything
#   ./scripts/deploy.sh --help       # Show this help
#
# Deployed addresses are persisted to .deployed-addresses after each successful
# deployment step.  Re-running the script after a partial failure will skip any
# step whose address was already saved.

set -euo pipefail

# ---------------------------------------------------------------------------
# 0. CLI / env setup
# ---------------------------------------------------------------------------

FRESH=false
RESUME=false

for arg in "$@"; do
  case "$arg" in
    --fresh)   FRESH=true ;;
    --resume)  RESUME=true ;;
    --help|-h)
      sed -n '2,12p' "$0" | sed 's/^# //'
      exit 0
      ;;
    *)
      echo "Unknown argument: $arg  (use --help for usage)"
      exit 1
      ;;
  esac
done

# Prefer a globally available `stellar` on PATH, fall back to a common WSL installation
if command -v stellar &> /dev/null; then
  STELLAR="stellar"
elif [ -f "/mnt/c/Program Files (x86)/Stellar CLI/stellar.exe" ]; then
  STELLAR="/mnt/c/Program Files (x86)/Stellar CLI/stellar.exe"
else
  echo "Error: stellar CLI not found on PATH or default Windows path."
  exit 1
fi

if [ -f .env ]; then
  source .env
else
  source .env.example
fi

# ---------------------------------------------------------------------------
# 1. Address persistence helpers
# ---------------------------------------------------------------------------

ADDRESSES_FILE=".deployed-addresses"

if [ "$FRESH" = true ]; then
  echo "=== --fresh flag set: removing saved addresses and starting clean ==="
  rm -f "$ADDRESSES_FILE"
fi

touch "$ADDRESSES_FILE"

# Write or overwrite a key in the addresses file
save_address() {
  local key="$1"
  local value="$2"
  # Remove any existing line for this key, then append
  if grep -q "^${key}=" "$ADDRESSES_FILE" 2>/dev/null; then
    # Use a temp file for portability (no in-place sed on all systems)
    grep -v "^${key}=" "$ADDRESSES_FILE" > "${ADDRESSES_FILE}.tmp" && mv "${ADDRESSES_FILE}.tmp" "$ADDRESSES_FILE"
  fi
  echo "${key}=${value}" >> "$ADDRESSES_FILE"
}

# Return the saved address for a key (empty string if not found)
load_address() {
  local key="$1"
  grep "^${key}=" "$ADDRESSES_FILE" 2>/dev/null | cut -d= -f2 || true
}

# ---------------------------------------------------------------------------
# 2. Transaction confirmation polling (replaces sleep 3)
# ---------------------------------------------------------------------------

# Wait until `stellar contract invoke` no longer returns a "transaction not found"
# style error, or until max retries are exhausted.
# For Stellar/Soroban the CLI itself handles submission retries, so what we
# actually need here is a brief back-off after a *successful* deploy call before
# the next dependent call.  We poll the contract's existence via `contract fetch`.
wait_for_contract() {
  local contract_id="$1"
  local max_attempts=15
  local attempt=0
  local delay=2

  echo "  Waiting for contract $contract_id to be confirmed on-chain..."
  while [ $attempt -lt $max_attempts ]; do
    if "$STELLAR" contract fetch \
        --id "$contract_id" \
        --network testnet \
        --output xdr > /dev/null 2>&1; then
      echo "  Confirmed."
      return 0
    fi
    attempt=$((attempt + 1))
    echo "  Attempt $attempt/$max_attempts — not yet visible, retrying in ${delay}s..."
    sleep "$delay"
  done

  echo "ERROR: Contract $contract_id was not confirmed after $((max_attempts * delay))s."
  echo "       Check your network connection or the Stellar testnet status."
  echo "       The address has NOT been saved.  Re-run the script to resume."
  return 1
}

# ---------------------------------------------------------------------------
# 3. Core deployment helper
# ---------------------------------------------------------------------------

# deploy_contract <KEY> <WASM_PATH>
# Deploys the wasm if KEY is not already in .deployed-addresses.
# Prints the contract ID and saves it.
deploy_contract() {
  local key="$1"
  local wasm="$2"

  local existing
  existing=$(load_address "$key")

  if [ -n "$existing" ]; then
    echo "  $key already deployed at $existing — skipping."
    echo "$existing"
    return 0
  fi

  if [ ! -f "$wasm" ]; then
    echo "ERROR: WASM file not found: $wasm"
    echo "       Run 'stellar contract build' first, or check the build output."
    return 1
  fi

  echo "  Deploying $key from $wasm ..."
  local contract_id
  if ! contract_id=$("$STELLAR" contract deploy \
      --wasm "$wasm" \
      --source "$DEPLOYER_ACCOUNT" \
      --network testnet 2>&1); then
    echo "ERROR: Deploy failed for $key:"
    echo "  $contract_id"
    echo "  Fix the error above and rerun — already-deployed contracts will be skipped."
    return 1
  fi

  # Confirm the contract is visible on-chain before saving
  if ! wait_for_contract "$contract_id"; then
    return 1
  fi

  save_address "$key" "$contract_id"
  echo "  Saved: $key=$contract_id"
  echo "$contract_id"
}

# ---------------------------------------------------------------------------
# 4. Initialization helper
# ---------------------------------------------------------------------------

# invoke_init <LABEL> <CONTRACT_ID> [-- args...]
# Runs `stellar contract invoke` and verifies the call succeeded.
# Skips if the KEY_initialized flag is already set in .deployed-addresses.
invoke_init() {
  local label="$1"
  local contract_id="$2"
  shift 2   # remaining args are passed verbatim to stellar contract invoke

  local init_key="${label}_initialized"
  local existing
  existing=$(load_address "$init_key")

  if [ -n "$existing" ]; then
    echo "  $label already initialized — skipping."
    return 0
  fi

  echo "  Initializing $label ($contract_id) ..."
  local output
  if ! output=$("$STELLAR" contract invoke \
      --id "$contract_id" \
      --source "$DEPLOYER_ACCOUNT" \
      --network testnet \
      "$@" 2>&1); then
    echo "ERROR: Initialization failed for $label:"
    echo "  $output"
    echo "  The contract is deployed but NOT initialized."
    echo "  Fix the error and rerun — this step will be retried automatically."
    return 1
  fi

  save_address "$init_key" "true"
  echo "  $label initialized successfully."
}

# ---------------------------------------------------------------------------
# 5. Pre-flight checks
# ---------------------------------------------------------------------------

echo ""
echo "=== Pre-flight checks ==="

if [ -z "${DEPLOYER_ACCOUNT:-}" ]; then
  echo "ERROR: DEPLOYER_ACCOUNT is not set.  Check your .env file."
  exit 1
fi

if [ -z "${USDC_ISSUER:-}" ]; then
  echo "ERROR: USDC_ISSUER is not set.  Check your .env file."
  exit 1
fi

if [ -z "${XLM_ASSET:-}" ]; then
  echo "ERROR: XLM_ASSET is not set.  Check your .env file."
  exit 1
fi

DEPLOYER_ADDRESS=$("$STELLAR" keys address "$DEPLOYER_ACCOUNT" 2>&1) || {
  echo "ERROR: Could not resolve address for key '$DEPLOYER_ACCOUNT'."
  echo "       Run scripts/setup-testnet.sh first."
  exit 1
}
echo "  Deployer address : $DEPLOYER_ADDRESS"
echo "  Addresses file   : $ADDRESSES_FILE"
echo "  Fresh deploy     : $FRESH"
echo ""

# ---------------------------------------------------------------------------
# 6. Build
# ---------------------------------------------------------------------------

echo "=== Building all contracts ==="
"$STELLAR" contract build
echo ""

# ---------------------------------------------------------------------------
# 7. Deploy & initialize all contracts
# ---------------------------------------------------------------------------

echo "=== Deploying registry_contract ==="
REGISTRY_ID=$(deploy_contract "registry" "target/wasm32v1-none/release/trusttrove_registry.wasm")
echo "Registry: $REGISTRY_ID"

invoke_init "registry" "$REGISTRY_ID" \
  -- initialize \
  --admin "$DEPLOYER_ADDRESS"

echo ""
echo "=== Deploying invoice_contract ==="
INVOICE_ID=$(deploy_contract "invoice" "target/wasm32v1-none/release/trusttrove_invoice.wasm")
echo "Invoice: $INVOICE_ID"

invoke_init "invoice" "$INVOICE_ID" \
  -- initialize \
  --admin "$DEPLOYER_ADDRESS" \
  --registry_contract "$REGISTRY_ID"

echo ""
echo "=== Deploying USDC escrow_contract ==="
ESCROW_USDC_ID=$(deploy_contract "escrow_usdc" "target/wasm32v1-none/release/trusttrove_escrow.wasm")
echo "USDC Escrow: $ESCROW_USDC_ID"

echo ""
echo "=== Deploying USDC pool_contract ==="
POOL_USDC_ID=$(deploy_contract "pool_usdc" "target/wasm32v1-none/release/trusttrove_pool.wasm")
echo "USDC Pool: $POOL_USDC_ID"

invoke_init "escrow_usdc" "$ESCROW_USDC_ID" \
  -- initialize \
  --admin "$DEPLOYER_ADDRESS" \
  --pool_contract "$POOL_USDC_ID" \
  --invoice_contract "$INVOICE_ID" \
  --usdc_asset "$USDC_ISSUER"

invoke_init "pool_usdc" "$POOL_USDC_ID" \
  -- initialize \
  --admin "$DEPLOYER_ADDRESS" \
  --invoice_contract "$INVOICE_ID" \
  --escrow_contract "$ESCROW_USDC_ID" \
  --usdc_asset "$USDC_ISSUER"

echo ""
echo "=== Deploying XLM escrow_contract ==="
ESCROW_XLM_ID=$(deploy_contract "escrow_xlm" "target/wasm32v1-none/release/trusttrove_escrow.wasm")
echo "XLM Escrow: $ESCROW_XLM_ID"

echo ""
echo "=== Deploying XLM pool_contract ==="
POOL_XLM_ID=$(deploy_contract "pool_xlm" "target/wasm32v1-none/release/trusttrove_pool.wasm")
echo "XLM Pool: $POOL_XLM_ID"

invoke_init "escrow_xlm" "$ESCROW_XLM_ID" \
  -- initialize \
  --admin "$DEPLOYER_ADDRESS" \
  --pool_contract "$POOL_XLM_ID" \
  --invoice_contract "$INVOICE_ID" \
  --usdc_asset "$XLM_ASSET"

invoke_init "pool_xlm" "$POOL_XLM_ID" \
  -- initialize \
  --admin "$DEPLOYER_ADDRESS" \
  --invoice_contract "$INVOICE_ID" \
  --escrow_contract "$ESCROW_XLM_ID" \
  --usdc_asset "$XLM_ASSET"

echo ""
echo "=== Wiring USDC pool_contract into invoice_contract ==="
invoke_init "invoice_set_pool" "$INVOICE_ID" \
  -- set_pool_contract \
  --pool_contract "$POOL_USDC_ID"

# ---------------------------------------------------------------------------
# 8. Persist final addresses to .deployed-addresses (already done per step)
#    and write a ready-to-use .env.deployed for the frontend
# ---------------------------------------------------------------------------

ENV_OUT=".env.deployed"
cat > "$ENV_OUT" <<EOF
# Generated by deploy.sh on $(date -u '+%Y-%m-%dT%H:%M:%SZ')
# Copy these values into trusttrove-app .env.local

NEXT_PUBLIC_REGISTRY_CONTRACT_ID=$REGISTRY_ID
NEXT_PUBLIC_INVOICE_CONTRACT_ID=$INVOICE_ID
NEXT_PUBLIC_ESCROW_USDC_CONTRACT_ID=$ESCROW_USDC_ID
NEXT_PUBLIC_ESCROW_XLM_CONTRACT_ID=$ESCROW_XLM_ID
NEXT_PUBLIC_POOL_USDC_CONTRACT_ID=$POOL_USDC_ID
NEXT_PUBLIC_POOL_XLM_CONTRACT_ID=$POOL_XLM_ID
EOF

echo ""
echo "==========================================="
echo "Deployment complete."
echo ""
echo "Addresses saved to: $ADDRESSES_FILE"
echo "Frontend env saved to: $ENV_OUT"
echo ""
echo "Add to trusttrove-app .env.local:"
echo ""
cat "$ENV_OUT"
echo "==========================================="
