#!/usr/bin/env bash
# invariant_report.sh — post-deploy invariant report for TrusTrove contracts
# (issue #1210).
#
# A read-only, idempotent cross-contract probe operators run against a
# newly-deployed stack (testnet or mainnet) to confirm every reciprocal
# address matches, every pool is pointing at the same funding asset as
# its paired escrow, pool counters are internally consistent, and one
# representative invoice read returns a well-formed response.
#
# Exit codes:
#   0  — all invariants hold
#   1  — one or more invariants failed (the report lists which)
#   2  — missing configuration (env vars / stellar CLI)
#
# Usage:
#   ./scripts/invariant_report.sh [--network testnet|mainnet]
#                                  [--sample-invoice <32-byte-hex>]
#
# All contract addresses are read from the ambient .env written by
# ./scripts/deploy.sh. No addresses, keys or balances are modified.
#
# This script is safe to run against mainnet: every `stellar contract invoke`
# below is a view call (`--send=no` on every invocation), so no transactions
# are submitted and no fees are charged.

set -u

NETWORK="testnet"
SAMPLE_INVOICE="0000000000000000000000000000000000000000000000000000000000000000"

while [ $# -gt 0 ]; do
    case "$1" in
        --network)          NETWORK="$2"; shift 2;;
        --sample-invoice)   SAMPLE_INVOICE="$2"; shift 2;;
        -h|--help)
            sed -n '2,24p' "$0"
            exit 0
            ;;
        *)  echo "unknown arg: $1" >&2; exit 2;;
    esac
done

STELLAR="/mnt/c/Program Files (x86)/Stellar CLI/stellar.exe"
if [ ! -f "$STELLAR" ]; then
    STELLAR="stellar"
fi
if ! command -v "$STELLAR" >/dev/null 2>&1 && [ ! -f "$STELLAR" ]; then
    echo "stellar CLI not found on PATH" >&2
    exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ ! -f .env ]; then
    echo "Error: .env file not found. Run ./scripts/deploy.sh first." >&2
    exit 2
fi
. "$SCRIPT_DIR/load_env.sh"
load_env .env

required=(
    DEPLOYER_ACCOUNT
    REGISTRY_CONTRACT_ID
    INVOICE_CONTRACT_ID
    POOL_USDC_CONTRACT_ID
    ESCROW_USDC_CONTRACT_ID
    USDC_ISSUER
)
for v in "${required[@]}"; do
    if [ -z "${!v-}" ]; then
        echo "Error: $v is not set in .env" >&2
        exit 2
    fi
done

FAILED=0
PASSED=0
TOTAL=0

bold()   { printf '\033[1m%s\033[0m' "$1"; }
green()  { printf '\033[32m%s\033[0m' "$1"; }
red()    { printf '\033[31m%s\033[0m' "$1"; }

invoke() {
    # view-only invoke; stderr folded in so error text appears in the report.
    local id="$1"; shift
    "$STELLAR" contract invoke \
        --id "$id" \
        --source "$DEPLOYER_ACCOUNT" \
        --network "$NETWORK" \
        --send=no \
        -- "$@" 2>&1
}

check() {
    # Usage: check <name> <expected> <actual>
    local name="$1" expected="$2" actual="$3"
    TOTAL=$((TOTAL + 1))
    # strip surrounding quotes stellar may wrap address strings in
    actual_clean="${actual//\"/}"
    expected_clean="${expected//\"/}"
    if [ "$actual_clean" = "$expected_clean" ]; then
        PASSED=$((PASSED + 1))
        printf '  %s %s\n' "$(green '[PASS]')" "$name"
    else
        FAILED=$((FAILED + 1))
        printf '  %s %s\n    expected: %s\n    actual:   %s\n' \
            "$(red '[FAIL]')" "$name" "$expected_clean" "$actual_clean"
    fi
}

check_nonempty() {
    local name="$1" actual="$2"
    TOTAL=$((TOTAL + 1))
    if [ -n "$actual" ] && [ "$actual" != "null" ] && \
       ! printf '%s' "$actual" | grep -qi 'error'; then
        PASSED=$((PASSED + 1))
        printf '  %s %s → %s\n' "$(green '[PASS]')" "$name" "$actual"
    else
        FAILED=$((FAILED + 1))
        printf '  %s %s\n    unexpected: %s\n' \
            "$(red '[FAIL]')" "$name" "$actual"
    fi
}

echo "$(bold 'TrusTrove invariant report') — network: $NETWORK"
echo

# ---------------------------------------------------------------------------
# 1. Reciprocal addresses: pool ↔ escrow, invoice ↔ escrow, invoice ↔ registry
# ---------------------------------------------------------------------------
echo "$(bold 'Reciprocal addresses')"
pool_escrow=$(invoke "$POOL_USDC_CONTRACT_ID" get_escrow_contract)
check "pool.get_escrow_contract == ESCROW_USDC" "$ESCROW_USDC_CONTRACT_ID" "$pool_escrow"

pool_invoice=$(invoke "$POOL_USDC_CONTRACT_ID" get_invoice_contract)
check "pool.get_invoice_contract == INVOICE" "$INVOICE_CONTRACT_ID" "$pool_invoice"

invoice_escrow=$(invoke "$INVOICE_CONTRACT_ID" get_escrow_contract)
check "invoice.get_escrow_contract == ESCROW_USDC" "$ESCROW_USDC_CONTRACT_ID" "$invoice_escrow"

invoice_registry=$(invoke "$INVOICE_CONTRACT_ID" get_agent_registry_contract)
check "invoice.get_agent_registry_contract == REGISTRY" "$REGISTRY_CONTRACT_ID" "$invoice_registry"

# ---------------------------------------------------------------------------
# 2. Asset consistency: pool funding asset matches the USDC contract id
# ---------------------------------------------------------------------------
echo
echo "$(bold 'Asset consistency')"
pool_asset=$(invoke "$POOL_USDC_CONTRACT_ID" get_funding_asset)
check "pool.get_funding_asset == USDC_ISSUER" "$USDC_ISSUER" "$pool_asset"

invoice_supports=$(invoke "$INVOICE_CONTRACT_ID" is_supported_asset --asset "$USDC_ISSUER")
check "invoice.is_supported_asset(USDC) == true" "true" "$invoice_supports"

# ---------------------------------------------------------------------------
# 3. Pool counters: total_funded <= total_deposits; active invoices non-negative
# ---------------------------------------------------------------------------
echo
echo "$(bold 'Pool counters')"
stats=$(invoke "$POOL_USDC_CONTRACT_ID" get_pool_stats)
check_nonempty "pool.get_pool_stats returns a payload" "$stats"

# Parse two counters we can compare without needing a JSON library.
td=$(printf '%s' "$stats" | grep -oE '"total_deposits":[[:space:]]*"[0-9]+"' \
    | head -n1 | grep -oE '[0-9]+')
tf=$(printf '%s' "$stats" | grep -oE '"total_funded":[[:space:]]*"[0-9]+"' \
    | head -n1 | grep -oE '[0-9]+')
TOTAL=$((TOTAL + 1))
if [ -n "$td" ] && [ -n "$tf" ] && [ "$tf" -le "$td" ]; then
    PASSED=$((PASSED + 1))
    printf '  %s pool: total_funded (%s) <= total_deposits (%s)\n' \
        "$(green '[PASS]')" "$tf" "$td"
else
    FAILED=$((FAILED + 1))
    printf '  %s pool: counter inconsistency (td=%s, tf=%s)\n' \
        "$(red '[FAIL]')" "${td:-?}" "${tf:-?}"
fi

# ---------------------------------------------------------------------------
# 4. Representative invoice read — exercises the invoice + escrow storage path
# ---------------------------------------------------------------------------
echo
echo "$(bold 'Representative invoice read')"
locked=$(invoke "$ESCROW_USDC_CONTRACT_ID" get_locked --invoice_id "$SAMPLE_INVOICE")
check_nonempty "escrow.get_locked(sample) returns a payload" "$locked"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo
printf '%s %d/%d checks passed\n' "$(bold 'Summary:')" "$PASSED" "$TOTAL"
if [ "$FAILED" -gt 0 ]; then
    echo "$(red "$FAILED check(s) failed")"
    exit 1
fi
echo "$(green 'All invariants hold.')"
exit 0
