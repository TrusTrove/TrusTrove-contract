#!/bin/bash
# deploy.sh — Idempotent deployment script for TrusTrove contracts
#
# issue #827: every contract is deployed with its protocol `__constructor`
# arguments, so a successful deploy is also a successful initialization —
# there is no separate `initialize` transaction anymore.
#
# Usage:
#   ./scripts/deploy.sh                         # Normal deploy (skips already-deployed contracts)
#   ./scripts/deploy.sh --resume                # Explicit resume mode (same as default)
#   ./scripts/deploy.sh --fresh                 # Ignore saved addresses and redeploy everything
#   ./scripts/deploy.sh --dry-run               # Show what would be deployed without actually deploying
#   ./scripts/deploy.sh --only <contract>       # Redeploy a single contract (hotfix mode)
#   ./scripts/deploy.sh --help                  # Show this help
#
# <contract> for --only must be one of:
#   registry, invoice, escrow_usdc, pool_usdc, usdc_pair,
#   escrow_xlm, pool_xlm, xlm_pair
#
# Because escrow and pool each receive the OTHER's address in their
# constructor, they are constructor-linked and must be redeployed together:
# --only escrow_usdc / --only pool_usdc is promoted to usdc_pair (and the
# XLM equivalents to xlm_pair).
#
# Deployed addresses are persisted to .deployed-addresses after each successful
# deployment step.  Re-running the script after a partial failure will skip any
# step whose address was already saved.  The pool deployment salt used to
# pre-compute the pool address that escrow is constructed against is persisted
# the same way (pool_usdc_salt / pool_xlm_salt), so a resumed run reconstructs
# the identical prediction.

set -euo pipefail

# Load configuration from .env (falling back to .env.example) using the safe
# line-by-line KEY=VALUE parser shared with verify.sh. Unlike `source`, it
# never executes arbitrary shell commands, so shell metacharacters in the
# file cannot be used for code execution (see CVE / issue #455, issue #612).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/load_env.sh"

if [ -f .env ]; then
  load_env .env
else
  load_env .env.example
fi

# ---------------------------------------------------------------------------
# 0. CLI / env setup
# ---------------------------------------------------------------------------

FRESH=false
RESUME=false
DRY_RUN=false
ONLY_CONTRACT=""

# Parse arguments, handling --only which consumes the next argument
_args=("$@")
_i=0
while [ $_i -lt ${#_args[@]} ]; do
  arg="${_args[$_i]}"
  case "$arg" in
    --fresh)   FRESH=true ;;
    --resume)  RESUME=true ;;
    --dry-run) DRY_RUN=true ;;
    --only)
      _i=$((_i + 1))
      if [ $_i -ge ${#_args[@]} ]; then
        echo "Error: --only requires a contract key argument."
        echo "Valid keys: registry, invoice, escrow_usdc, pool_usdc, usdc_pair, escrow_xlm, pool_xlm, xlm_pair"
        exit 1
      fi
      ONLY_CONTRACT="${_args[$_i]}"
      case "$ONLY_CONTRACT" in
        registry|invoice) ;;
        escrow_usdc|pool_usdc)
          # Constructor-linked pair: redeploying one requires redeploying the other.
          echo "Note: --only $ONLY_CONTRACT promotes to --only usdc_pair (issue #827 constructor linkage)."
          ONLY_CONTRACT="usdc_pair"
          ;;
        escrow_xlm|pool_xlm)
          echo "Note: --only $ONLY_CONTRACT promotes to --only xlm_pair (issue #827 constructor linkage)."
          ONLY_CONTRACT="xlm_pair"
          ;;
        usdc_pair|xlm_pair) ;;
        *)
          echo "Error: unknown contract key '$ONLY_CONTRACT' for --only."
          echo "Valid keys: registry, invoice, escrow_usdc, pool_usdc, usdc_pair, escrow_xlm, pool_xlm, xlm_pair"
          exit 1
          ;;
      esac
      ;;
    --help|-h)
      sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "Unknown argument: $arg  (use --help for usage)"
      exit 1
      ;;
  esac
  _i=$((_i + 1))
done

# Prefer a globally available `stellar` on PATH, fall back to STELLAR_BIN env var,
# then fall back to a common WSL installation
if command -v stellar &> /dev/null; then
  STELLAR="stellar"
elif [ -n "${STELLAR_BIN:-}" ] && [ -f "$STELLAR_BIN" ]; then
  STELLAR="$STELLAR_BIN"
elif [ -f "/mnt/c/Program Files (x86)/Stellar CLI/stellar.exe" ]; then
  STELLAR="/mnt/c/Program Files (x86)/Stellar CLI/stellar.exe"
else
  echo "Error: stellar CLI not found."
  echo ""
  echo "Try one of:"
  echo "  1. Install stellar CLI globally (https://developers.stellar.org/docs/learn/developing-with-soroban/setup)"
  echo "  2. Set STELLAR_BIN=/path/to/stellar.exe"
  echo "  3. On WSL, ensure 'Stellar CLI' is installed in Program Files (x86)"
  echo ""
  exit 1
fi

if [ -f .env ]; then
  load_env .env
else
  load_env .env.example
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

# deploy_contract <KEY> <WASM_PATH> <SALT_HEX_OR_EMPTY> [CONSTRUCTOR_ARGS...]
# Deploys the wasm if KEY is not already in .deployed-addresses, invoking the
# contract's protocol `__constructor` with CONSTRUCTOR_ARGS in the SAME
# transaction (issue #827: deploy and initialization are atomic; a deployed
# contract is always constructed).
# SALT_HEX_OR_EMPTY: optional 32-byte hex salt fixing the deploy address.
# The stellar CLI derives a constructor deploy's contract id from
# deployer+salt only, so passing a salt lets the address be predicted
# up-front (see predict_contract_id) — used to construct escrow against the
# pool address before the pool itself is deployed.
# Prints the contract ID and saves it.
deploy_contract() {
  local key="$1"
  local wasm="$2"
  local salt="$3"
  shift 3

  local existing
  existing=$(load_address "$key")

  if [ -n "$existing" ]; then
    echo "  $key already deployed at $existing — skipping." 1>&2
    echo "$existing"
    return 0
  fi

  if [ "$DRY_RUN" = true ]; then
    {
      echo "  [DRY-RUN] Would deploy $key from $wasm"
      if [ -n "$salt" ]; then echo "  [DRY-RUN]   salt: $salt"; fi
      echo "  [DRY-RUN]   __constructor args: $*"
    } 1>&2
    echo "<dry-run>"
    return 0
  fi

  if [ ! -f "$wasm" ]; then
    echo "ERROR: WASM file not found: $wasm" 1>&2
    echo "       Run 'stellar contract build' first, or check the build output." 1>&2
    return 1
  fi

  echo "  Deploying $key from $wasm ..." 1>&2
  local -a cmd=("$STELLAR" contract deploy
      --wasm "$wasm"
      --source "$DEPLOYER_ACCOUNT"
      --network testnet)
  if [ -n "$salt" ]; then
    cmd+=(--salt "$salt")
  fi
  if [ "$#" -gt 0 ]; then
    # Positional args after `--` are forwarded verbatim to __constructor.
    cmd+=(-- "$@")
  fi

  local output contract_id
  if ! output=$("${cmd[@]}" 2>&1); then
    {
      echo "ERROR: Deploy failed for $key:"
      echo "  $output"
      echo "  Fix the error above and rerun — already-deployed contracts will be skipped."
    } 1>&2
    return 1
  fi

  # Extract the contract id from the CLI output (the CLI prints INFO noise on
  # stderr which is merged above, so grep the strkey rather than trusting raw stdout).
  contract_id=$(printf '%s\n' "$output" | grep -oE 'C[A-Z2-7]{55}' | tail -1)
  if [ -z "$contract_id" ]; then
    echo "ERROR: Deploy for $key succeeded but no contract id was found in output:" 1>&2
    echo "  $output" 1>&2
    return 1
  fi

  # Confirm the contract is visible on-chain before saving
  if ! wait_for_contract "$contract_id" 1>&2; then
    return 1
  fi

  save_address "$key" "$contract_id"
  echo "  Saved: $key=$contract_id" 1>&2
  echo "$contract_id"
}

# ---------------------------------------------------------------------------
# 3b. Address prediction for constructor-linked deploys (issue #827)
# ---------------------------------------------------------------------------

# get_or_create_salt <KEY>
# Returns the persisted deploy salt for <KEY> (saved as <KEY>_salt in
# .deployed-addresses), generating and persisting a fresh 32-byte hex salt
# on first use.  Persisting BEFORE deploying escrow/pool is what makes a
# resumed run reconstruct the same prediction.
get_or_create_salt() {
  local key="$1"
  local salt
  salt=$(load_address "${key}_salt")
  if [ -z "$salt" ]; then
    salt=$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')
    save_address "${key}_salt" "$salt"
  fi
  echo "$salt"
}

# predict_contract_id <SALT_HEX>
# Computes the contract id that `deploy_contract ... <SALT_HEX> -- ...` with
# the deployer account will produce.  Both the CLI's deploy and this call use
# the same FROM_ADDRESS (deployer, salt) preimage; the constructor arguments
# do NOT enter the id derivation, so this prediction is exact.
predict_contract_id() {
  local id
  id=$("$STELLAR" contract id wasm \
      --salt "$1" \
      --source-account "$DEPLOYER_ADDRESS" \
      --network testnet 2>/dev/null | grep -oE 'C[A-Z2-7]{55}' | tail -1)
  if [ -z "$id" ]; then
    echo "ERROR: could not predict contract id for salt $1" >&2
    return 1
  fi
  echo "$id"
}

# ---------------------------------------------------------------------------
# 4. Post-deploy wiring helper
# ---------------------------------------------------------------------------

# invoke_init <LABEL> <CONTRACT_ID> [-- fn args...]
# Runs `stellar contract invoke` for a post-deploy wiring call (invoice
# set_pool_contract / set_escrow_contract / add_supported_asset / ...).
# Since issue #827 the constructor replaces initialize, so this helper is no
# longer used for initialization — only for the optional wiring transactions,
# with the <LABEL>_initialized marker retained for resume compatibility.
invoke_init() {
  local label="$1"
  local contract_id="$2"
  shift 2   # remaining args are passed verbatim to stellar contract invoke

  local init_key="${label}_initialized"
  local existing
  existing=$(load_address "$init_key")

  if [ -n "$existing" ]; then
    echo "  $label already applied — skipping."
    return 0
  fi

  if [ "$DRY_RUN" = true ]; then
    echo "  [DRY-RUN] Would invoke on $label ($contract_id): $*"
    return 0
  fi

  echo "  Invoking $label ($contract_id) ..."
  local output
  if ! output=$("$STELLAR" contract invoke \
      --id "$contract_id" \
      --source "$DEPLOYER_ACCOUNT" \
      --network testnet \
      "$@" 2>&1); then
    echo "ERROR: Invocation failed for $label:"
    echo "  $output"
    # Issue #825: distinguish a front-run initialization from a transient
    # error. `initialize` panics with AlreadyInitialized (#1) if the
    # storage Admin key is set — and until this transaction lands, ANY
    # address can initialize a fresh deployment with their own admin. A
    # rerun can never win that race retroactively, so saying "rerun" here
    # would be misleading. Constructor-migrated contracts (#827) reach this
    # path only for the post-deploy wiring calls, so the generic branch
    # speaks about the wiring step.
    case "$output" in
      *AlreadyInitialized*|*"Error(Contract, #1)"*)
        echo "  This contract was ALREADY INITIALIZED — possibly by someone"
        echo "  other than this deployer (front-run initialization, issue #825)."
        echo "  Rerunning cannot fix it: check who holds admin with"
        echo "  scripts/verify.sh (it compares get_admin against the deployer)"
        echo "  and redeploy the contract with --fresh. See DEPLOYMENT.md,"
        echo "  'Front-run initialization detection'."
        ;;
      *)
        echo "  The contract is deployed but this wiring step did not complete."
        echo "  Fix the error and rerun — this step will be retried automatically."
        ;;
    esac
    return 1
  fi

  save_address "$init_key" "true"
  echo "  $label applied successfully."
}

# ---------------------------------------------------------------------------
# 4b. Post-initialize admin verification (issue #825)
# ---------------------------------------------------------------------------

# verify_admin <LABEL> <CONTRACT_ID>
# Reads get_admin back from the contract just initialized and aborts unless
# it equals $DEPLOYER_ADDRESS. `initialize` only authorizes whatever admin
# is passed in, so a front-runner who initialized first with their own
# address would otherwise go unnoticed — our own initialize would simply
# fail with AlreadyInitialized and the deploy artifacts would be poisoned.
# Every contract this script deploys (registry, invoice, escrow, pool)
# exposes get_admin.
verify_admin() {
  local label="$1"
  local contract_id="$2"

  if [ "$DRY_RUN" = true ]; then
    echo "  [DRY-RUN] Would assert get_admin == $DEPLOYER_ADDRESS for $label"
    return 0
  fi

  # Skip contracts that were deployed in dry-run mode (no real address).
  case "$contract_id" in
    ""|"<dry-run>") return 0 ;;
  esac

  local on_chain_admin
  if ! on_chain_admin=$("$STELLAR" contract invoke \
      --id "$contract_id" \
      --source "$DEPLOYER_ACCOUNT" \
      --network testnet \
      -- get_admin 2>&1); then
    echo "ERROR: Could not read on-chain admin for $label ($contract_id):"
    echo "  $on_chain_admin"
    echo "  POSSIBLE FRONT-RUN INITIALIZATION or uninitialized contract."
    echo "  Do NOT use this deployment — redeploy with --fresh"
    echo "  (see DEPLOYMENT.md, 'Front-run initialization detection')."
    exit 1
  fi
  on_chain_admin="$(printf '%s' "$on_chain_admin" | tr -d '"' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"

  if [ "$on_chain_admin" != "$DEPLOYER_ADDRESS" ]; then
    echo "ERROR: $label ($contract_id) reports admin '$on_chain_admin',"
    echo "  but this deployer is '$DEPLOYER_ADDRESS'."
    echo "  POSSIBLE FRONT-RUN INITIALIZATION: someone else initialized this"
    echo "  contract with their own admin before our initialize landed."
    echo "  Do NOT use this deployment — redeploy with --fresh"
    echo "  (see DEPLOYMENT.md, 'Front-run initialization detection')."
    exit 1
  fi
  echo "  [OK] $label on-chain admin verified: $on_chain_admin"
}

# ---------------------------------------------------------------------------
# 4c. Post-wiring invoice pin verification (issue #998)
# ---------------------------------------------------------------------------

# verify_pinned_invoice <LABEL> <ESCROW_ID> <EXPECTED_INVOICE_ID>
# Asserts that escrow's stored invoice contract reference matches the expected
# invoice contract address, preventing surviving escrows from retaining a stale
# invoice pin after invoice-only redeployment.
verify_pinned_invoice() {
  local label="$1"
  local escrow_id="$2"
  local expected_invoice="$3"

  if [ "$DRY_RUN" = true ]; then
    echo "  [DRY-RUN] Would assert get_invoice_contract == $expected_invoice for $label"
    return 0
  fi

  # Skip contracts that were deployed in dry-run mode (no real address).
  case "$escrow_id" in
    ""|"<dry-run>") return 0 ;;
  esac

  local on_chain_invoice
  if ! on_chain_invoice=$("$STELLAR" contract invoke \
      --id "$escrow_id" \
      --source "$DEPLOYER_ACCOUNT" \
      --network testnet \
      -- get_invoice_contract 2>&1); then
    echo "ERROR: Could not read on-chain invoice contract for $label ($escrow_id):"
    echo "  $on_chain_invoice"
    exit 1
  fi
  on_chain_invoice="$(printf '%s' "$on_chain_invoice" | tr -d '"' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"

  if [ "$on_chain_invoice" != "$expected_invoice" ]; then
    echo "ERROR: $label ($escrow_id) reports invoice contract '$on_chain_invoice',"
    echo "  but expected '$expected_invoice'."
    echo "  Surviving escrow is pinned to stale invoice address (issue #998)."
    exit 1
  fi
  echo "  [OK] $label on-chain invoice contract verified: $on_chain_invoice"
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
echo "  Dry-run mode     : $DRY_RUN"
if [ -n "$ONLY_CONTRACT" ]; then echo "  Only deploying   : $ONLY_CONTRACT"; fi
echo ""

# ---------------------------------------------------------------------------
# 6. Build
# ---------------------------------------------------------------------------

if [ "$DRY_RUN" = true ]; then
  echo "=== [DRY-RUN] Skipping build ==="
else
  echo "=== Building all contracts ==="
  "$STELLAR" contract build
fi
echo ""

# ---------------------------------------------------------------------------
# 7. Deploy all contracts (each deploy atomically runs its __constructor)
# ---------------------------------------------------------------------------

# In --only mode, load all existing saved addresses first so cross-contract
# references remain valid even when only one contract is redeployed.
_load_or_deploy() {
  local key="$1"
  local var="$2"
  local existing
  existing=$(load_address "$key")
  if [ -n "$existing" ]; then
    eval "$var=\$existing"
  fi
}

if [ -n "$ONLY_CONTRACT" ]; then
  echo "=== --only mode: redeploying: $ONLY_CONTRACT ==="
  # Clear the saved addresses for the targeted step(s) so deploy_contract
  # re-runs them.  Since issue #827 escrow+pool are constructor-linked (each
  # receives the other's address), a pool/escrow redeploy clears the whole
  # pair, its persisted pool salt, and the invoice wiring flags that point at
  # the pair being replaced.  An invoice redeploy clears the wiring flags too
  # (they belong to the old invoice instance).
  _clear_keys=( "$ONLY_CONTRACT" )
  case "$ONLY_CONTRACT" in
    usdc_pair)
      _clear_keys=( escrow_usdc pool_usdc pool_usdc_salt invoice_set_pool_initialized invoice_set_escrow_initialized escrow_set_invoice_initialized )
      ;;
    xlm_pair)
      _clear_keys=( escrow_xlm pool_xlm pool_xlm_salt invoice_set_pool_initialized invoice_set_escrow_initialized escrow_xlm_set_invoice_initialized )
      ;;
    invoice)
      _clear_keys=( invoice invoice_set_pool_initialized invoice_set_escrow_initialized \
                    escrow_set_invoice_initialized escrow_xlm_set_invoice_initialized \
                    invoice_add_asset_usdc_initialized invoice_add_asset_xlm_initialized \
                    invoice_set_agent_registry_initialized )
      ;;
  esac
  # Also drop any legacy <key>_initialized markers left by pre-#827 runs.
  for _k in "${_clear_keys[@]}"; do
    _clear_keys+=("${_k}_initialized")
  done
  _pattern=$(printf '%s|' "${_clear_keys[@]}")
  _pattern="${_pattern%|}"
  grep -vE "^(${_pattern})=" "$ADDRESSES_FILE" > "${ADDRESSES_FILE}.tmp" 2>/dev/null || true
  mv "${ADDRESSES_FILE}.tmp" "$ADDRESSES_FILE"

  # Load all surviving saved addresses into variables for cross-refs
  REGISTRY_ID=$(load_address "registry")
  INVOICE_ID=$(load_address "invoice")
  ESCROW_USDC_ID=$(load_address "escrow_usdc")
  POOL_USDC_ID=$(load_address "pool_usdc")
  ESCROW_XLM_ID=$(load_address "escrow_xlm")
  POOL_XLM_ID=$(load_address "pool_xlm")
  echo "  Existing saved addresses loaded for cross-contract references."
  echo ""
fi

# Helper: returns true if we should run this step.
# In --only mode only the targeted step(s) run; otherwise all steps run.
# escrow_usdc/pool_usdc share the usdc_pair gate (constructor linkage), as do
# escrow_xlm/pool_xlm under xlm_pair.
_should_run() {
  case "${1}" in
    escrow_usdc|pool_usdc)
      [ -z "$ONLY_CONTRACT" ] || [ "$ONLY_CONTRACT" = "usdc_pair" ]
      ;;
    escrow_xlm|pool_xlm)
      [ -z "$ONLY_CONTRACT" ] || [ "$ONLY_CONTRACT" = "xlm_pair" ]
      ;;
    *)
      [ -z "$ONLY_CONTRACT" ] || [ "$ONLY_CONTRACT" = "$1" ]
      ;;
  esac
}

if _should_run "registry"; then
  echo "=== Deploying registry_contract (constructor runs inline) ==="
  REGISTRY_ID=$(deploy_contract "registry" \
    "target/wasm32v1-none/release/trusttrove_registry.wasm" \
    "" \
    --admin "$DEPLOYER_ADDRESS")
  echo "Registry: $REGISTRY_ID"
  verify_admin "registry" "$REGISTRY_ID"
  echo ""
fi

if _should_run "invoice"; then
  echo "=== Deploying invoice_contract (constructor runs inline) ==="
  if [ -z "${REGISTRY_ID:-}" ] && [ "$DRY_RUN" != true ]; then
    echo "ERROR: invoice's constructor needs the registry address,"
    echo "       but no registry deployment is recorded. Deploy registry first."
    exit 1
  fi
  INVOICE_ID=$(deploy_contract "invoice" \
    "target/wasm32v1-none/release/trusttrove_invoice.wasm" \
    "" \
    --admin "$DEPLOYER_ADDRESS" \
    --registry_contract "$REGISTRY_ID")
  echo "Invoice: $INVOICE_ID"
  verify_admin "invoice" "$INVOICE_ID"
  echo ""
fi

# ---------------------------------------------------------------------------
# 7b. USDC escrow/pool pair — constructor-linked via a pre-computed pool id
#
# escrow's constructor receives the pool address and pool's constructor
# receives the escrow address.  The deploy address is derived from
# (deployer, salt) and does not depend on the constructor args, so we persist
# a salt for the pool, predict its future address, construct escrow against
# that prediction, then deploy the pool AT that same salt/address.
# ---------------------------------------------------------------------------

if _should_run "escrow_usdc"; then
  echo "=== Deploying USDC escrow_contract (constructed against predicted pool id) ==="
  POOL_USDC_SALT=$(get_or_create_salt "pool_usdc")
  if [ "$DRY_RUN" = true ]; then
    POOL_USDC_TARGET_ID="<predicted-pool-usdc-id>"
  else
    POOL_USDC_TARGET_ID=$(load_address "pool_usdc")
    if [ -z "$POOL_USDC_TARGET_ID" ]; then
      POOL_USDC_TARGET_ID=$(predict_contract_id "$POOL_USDC_SALT")
    fi
  fi
  ESCROW_USDC_ID=$(deploy_contract "escrow_usdc" \
    "target/wasm32v1-none/release/trusttrove_escrow.wasm" \
    "" \
    --admin "$DEPLOYER_ADDRESS" \
    --pool_contract "$POOL_USDC_TARGET_ID" \
    --usdc_asset "$USDC_ISSUER")
  echo "USDC Escrow: $ESCROW_USDC_ID"
  verify_admin "escrow_usdc" "$ESCROW_USDC_ID"
  echo ""
fi

if _should_run "pool_usdc"; then
  echo "=== Deploying USDC pool_contract (single step, no separate init) ==="
  if [ -z "${POOL_USDC_SALT:-}" ]; then
    # pool-only redeploy without the escrow step above (e.g. --only usdc_pair
    # where escrow was skipped is impossible since the pair gate runs both,
    # but the variables may be unset if this block runs standalone).
    POOL_USDC_SALT=$(get_or_create_salt "pool_usdc")
    POOL_USDC_TARGET_ID=$(load_address "pool_usdc")
    if [ -z "$POOL_USDC_TARGET_ID" ] && [ "$DRY_RUN" != true ]; then
      POOL_USDC_TARGET_ID=$(predict_contract_id "$POOL_USDC_SALT")
    fi
  fi
  if [ "$DRY_RUN" != true ] && [ -z "${ESCROW_USDC_ID:-}" ]; then
    ESCROW_USDC_ID=$(load_address "escrow_usdc")
    if [ -z "$ESCROW_USDC_ID" ]; then
      echo "ERROR: pool's constructor needs the escrow address, but none is recorded."
      exit 1
    fi
  fi
  POOL_USDC_ID=$(deploy_contract "pool_usdc" \
    "target/wasm32v1-none/release/trusttrove_pool.wasm" \
    "$POOL_USDC_SALT" \
    --admin "$DEPLOYER_ADDRESS" \
    --invoice_contract "$INVOICE_ID" \
    --escrow_contract "$ESCROW_USDC_ID" \
    --funding_asset "$USDC_ISSUER" \
    --registry_contract "$REGISTRY_ID" \
    --treasury "$DEPLOYER_ADDRESS" \
    --min_initial_deposit 10000000 \
    --share_name '"TrusTrove USDC Pool Shares"' \
    --share_symbol '"TT-USDC"' \
    --share_decimals 7)
  # The pool MUST land on the address escrow was constructed against.
  if [ "$DRY_RUN" != true ] && [ -n "${POOL_USDC_TARGET_ID:-}" ] \
     && [ "$POOL_USDC_ID" != "$POOL_USDC_TARGET_ID" ]; then
    echo "ERROR: pool_usdc deployed at $POOL_USDC_ID but escrow_usdc was"
    echo "       constructed against $POOL_USDC_TARGET_ID. Aborting before"
    echo "       saving inconsistent addresses. Investigate the salt in"
    echo "       $ADDRESSES_FILE (pool_usdc_salt) and redeploy the pair."
    exit 1
  fi
  echo "USDC Pool: $POOL_USDC_ID"
  verify_admin "pool_usdc" "$POOL_USDC_ID"
  echo ""
fi

if _should_run "escrow_xlm"; then
  echo "=== Deploying XLM escrow_contract (EXPERIMENTAL, constructed against predicted pool id) ==="
  POOL_XLM_SALT=$(get_or_create_salt "pool_xlm")
  if [ "$DRY_RUN" = true ]; then
    POOL_XLM_TARGET_ID="<predicted-pool-xlm-id>"
  else
    POOL_XLM_TARGET_ID=$(load_address "pool_xlm")
    if [ -z "$POOL_XLM_TARGET_ID" ]; then
      POOL_XLM_TARGET_ID=$(predict_contract_id "$POOL_XLM_SALT")
    fi
  fi
  ESCROW_XLM_ID=$(deploy_contract "escrow_xlm" \
    "target/wasm32v1-none/release/trusttrove_escrow.wasm" \
    "" \
    --admin "$DEPLOYER_ADDRESS" \
    --pool_contract "$POOL_XLM_TARGET_ID" \
    --usdc_asset "$XLM_ASSET")
  echo "XLM Escrow: $ESCROW_XLM_ID"
  verify_admin "escrow_xlm" "$ESCROW_XLM_ID"
  echo ""
fi

if _should_run "pool_xlm"; then
  echo "=== Deploying XLM pool_contract (EXPERIMENTAL, single step, no separate init) ==="
  if [ -z "${POOL_XLM_SALT:-}" ]; then
    POOL_XLM_SALT=$(get_or_create_salt "pool_xlm")
    POOL_XLM_TARGET_ID=$(load_address "pool_xlm")
    if [ -z "$POOL_XLM_TARGET_ID" ] && [ "$DRY_RUN" != true ]; then
      POOL_XLM_TARGET_ID=$(predict_contract_id "$POOL_XLM_SALT")
    fi
  fi
  if [ "$DRY_RUN" != true ] && [ -z "${ESCROW_XLM_ID:-}" ]; then
    ESCROW_XLM_ID=$(load_address "escrow_xlm")
    if [ -z "$ESCROW_XLM_ID" ]; then
      echo "ERROR: pool's constructor needs the escrow address, but none is recorded."
      exit 1
    fi
  fi
  POOL_XLM_ID=$(deploy_contract "pool_xlm" \
    "target/wasm32v1-none/release/trusttrove_pool.wasm" \
    "$POOL_XLM_SALT" \
    --admin "$DEPLOYER_ADDRESS" \
    --invoice_contract "$INVOICE_ID" \
    --escrow_contract "$ESCROW_XLM_ID" \
    --funding_asset "$XLM_ASSET" \
    --registry_contract "$REGISTRY_ID" \
    --treasury "$DEPLOYER_ADDRESS" \
    --min_initial_deposit 10000000 \
    --share_name '"TrusTrove XLM Pool Shares"' \
    --share_symbol '"TT-XLM"' \
    --share_decimals 7)
  if [ "$DRY_RUN" != true ] && [ -n "${POOL_XLM_TARGET_ID:-}" ] \
     && [ "$POOL_XLM_ID" != "$POOL_XLM_TARGET_ID" ]; then
    echo "ERROR: pool_xlm deployed at $POOL_XLM_ID but escrow_xlm was"
    echo "       constructed against $POOL_XLM_TARGET_ID. Aborting before"
    echo "       saving inconsistent addresses. Investigate the salt in"
    echo "       $ADDRESSES_FILE (pool_xlm_salt) and redeploy the pair."
    exit 1
  fi
  echo "XLM Pool: $POOL_XLM_ID"
  verify_admin "pool_xlm" "$POOL_XLM_ID"
  echo ""
fi

if [ -z "$ONLY_CONTRACT" ] || [ "$ONLY_CONTRACT" = "usdc_pair" ]; then
  echo ""
  echo "=== Wiring USDC pool_contract into invoice_contract ==="
  invoke_init "invoice_set_pool" "$INVOICE_ID" \
    -- set_pool_contract \
    --pool_contract "$POOL_USDC_ID"
fi

# Without the escrow wiring below, repay / repay_partial / repay_early panic
# with InvoiceError::NotFound when reading DataKey::EscrowContract.
echo ""
echo "=== Wiring USDC escrow_contract into invoice_contract ==="
invoke_init "invoice_set_escrow" "$INVOICE_ID" \
  -- set_escrow_contract \
  --escrow_contract "$ESCROW_USDC_ID"

if [ -z "$ONLY_CONTRACT" ] || [ "$ONLY_CONTRACT" = "usdc_pair" ] || [ "$ONLY_CONTRACT" = "invoice" ]; then
  if [ -n "${ESCROW_USDC_ID:-}" ]; then
    echo ""
    echo "=== Wiring USDC invoice_contract into escrow_contract ==="
    invoke_init "escrow_set_invoice" "$ESCROW_USDC_ID" \
      -- set_invoice_contract \
      --invoice_contract "$INVOICE_ID"
    verify_pinned_invoice "escrow_usdc" "$ESCROW_USDC_ID" "$INVOICE_ID"
  fi
fi

if [ -z "$ONLY_CONTRACT" ] || [ "$ONLY_CONTRACT" = "xlm_pair" ] || [ "$ONLY_CONTRACT" = "invoice" ]; then
  if [ -n "${ESCROW_XLM_ID:-}" ]; then
    echo ""
    echo "=== Wiring XLM invoice_contract into escrow_contract ==="
    invoke_init "escrow_xlm_set_invoice" "$ESCROW_XLM_ID" \
      -- set_invoice_contract \
      --invoice_contract "$INVOICE_ID"
    verify_pinned_invoice "escrow_xlm" "$ESCROW_XLM_ID" "$INVOICE_ID"
  fi
fi

# Without allow-listing, create rejects every invoice with UnsupportedAsset.
# add_supported_asset is itself idempotent on-chain (a no-op for an asset
# that is already allow-listed).
echo ""
echo "=== Allow-listing USDC as a supported funding asset ==="
invoke_init "invoice_add_asset_usdc" "$INVOICE_ID" \
  -- add_supported_asset \
  --asset "$USDC_ISSUER"

echo ""
echo "=== Allow-listing XLM as a supported funding asset (EXPERIMENTAL) ==="
invoke_init "invoice_add_asset_xlm" "$INVOICE_ID" \
  -- add_supported_asset \
  --asset "$XLM_ASSET"

# Agent-registry wiring is optional: it is only performed when
# AGENT_REGISTRY_CONTRACT is set in .env / .env.example.  Without it,
# submit_attestation panics with InvoiceError::NotFound and
# list_for_financing can never unlock.
if [ -n "${AGENT_REGISTRY_CONTRACT:-}" ]; then
  echo ""
  echo "=== Wiring agent_registry_contract into invoice_contract ==="
  invoke_init "invoice_set_agent_registry" "$INVOICE_ID" \
    -- set_agent_registry_contract \
    --agent_registry_contract "$AGENT_REGISTRY_CONTRACT"
else
  echo ""
  echo "=== AGENT_REGISTRY_CONTRACT is not set — skipping agent-registry wiring ==="
  echo "    submit_attestation will fail until it is wired (see DEPLOYMENT.md)."
fi

# ---------------------------------------------------------------------------
# 8. Persist final addresses to .deployed-addresses (already done per step)
#    and write a ready-to-use .env.deployed for the frontend
# ---------------------------------------------------------------------------

if [ "$DRY_RUN" = true ]; then
  echo "=== [DRY-RUN] Skipping .env.deployed generation ==="
else
ENV_OUT=".env.deployed"
cat > "$ENV_OUT" <<EOF
# Generated by deploy.sh on $(date -u '+%Y-%m-%dT%H:%M:%SZ')
# Copy these values into trusttrove-app .env.local

NEXT_PUBLIC_REGISTRY_CONTRACT_ID=$REGISTRY_ID
NEXT_PUBLIC_INVOICE_CONTRACT_ID=$INVOICE_ID
NEXT_PUBLIC_ESCROW_USDC_CONTRACT_ID=$ESCROW_USDC_ID
NEXT_PUBLIC_ESCROW_XLM_CONTRACT_ID=$ESCROW_XLM_ID # EXPERIMENTAL
NEXT_PUBLIC_POOL_USDC_CONTRACT_ID=$POOL_USDC_ID
NEXT_PUBLIC_POOL_XLM_CONTRACT_ID=$POOL_XLM_ID # EXPERIMENTAL
EOF

JSON_OUT="deployments.json"
cat > "$JSON_OUT" <<EOF
{
  "registry": "$REGISTRY_ID",
  "invoice": "$INVOICE_ID",
  "escrow_usdc": "$ESCROW_USDC_ID",
  "escrow_xlm": "$ESCROW_XLM_ID",
  "pool_usdc": "$POOL_USDC_ID",
  "pool_xlm": "$POOL_XLM_ID",
  "updated_at": "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
}
EOF

if [ -f "scripts/maintainer/update-readme-addresses.sh" ]; then
  bash scripts/maintainer/update-readme-addresses.sh
fi

echo ""
echo "==========================================="
echo "Deployment complete."
echo ""
echo "Addresses saved to: $ADDRESSES_FILE"
echo "JSON addresses saved to: $JSON_OUT"
echo "Frontend env saved to: $ENV_OUT"
echo ""
echo "Add to trusttrove-app .env.local:"
echo ""
cat "$ENV_OUT"
echo "==========================================="
fi
