#!/bin/bash
set -euo pipefail

# Structural validation of the deployed-addresses table in README.md.
#
# Checks the Markdown table between the markers
# <!-- START_DEPLOYED_ADDRESSES --> and <!-- END_DEPLOYED_ADDRESSES -->:
#   - both markers present exactly once (START before END),
#   - the table has the expected "Contract | Address" header and separator,
#   - the four expected contract rows (registry, invoice, escrow, pool) are
#     each present exactly once, with no extra or duplicate rows,
#   - every address is a backtick-quoted Stellar contract ID: "C" followed
#     by 55 characters from the base32 alphabet, i.e. C[A-Z2-7]{55}.
#
# NOTE: this is a STRUCTURAL check only. It does NOT verify that the recorded
# addresses are live or correct on-chain — the repo keeps no committed source
# of truth to compare against (.deployed-addresses and deployments.json are
# gitignored). See DEPLOYMENT.md, "Contract Address Lifecycle & Rotation
# Policy".
#
# Usage: scripts/maintainer/check-readme-addresses.sh [PATH_TO_README]

fail() {
  echo "check-readme-addresses: ERROR: $*" >&2
  exit 1
}

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
README_PATH="${1:-$REPO_ROOT/README.md}"

[ -f "$README_PATH" ] || fail "$README_PATH not found"

START_MARKER='<!-- START_DEPLOYED_ADDRESSES -->'
END_MARKER='<!-- END_DEPLOYED_ADDRESSES -->'
ADDRESS_RE='^`C[A-Z2-7]{55}`$'
EXPECTED_CONTRACTS="registry_contract invoice_contract escrow_contract pool_contract"

start_count=$(grep -cF -- "$START_MARKER" "$README_PATH" || true)
end_count=$(grep -cF -- "$END_MARKER" "$README_PATH" || true)
[ "$start_count" = "1" ] || fail "expected exactly 1 $START_MARKER marker, found $start_count"
[ "$end_count" = "1" ] || fail "expected exactly 1 $END_MARKER marker, found $end_count"

TABLE=$(sed -n "/$(printf '%s' "$START_MARKER" | sed 's/[][\\\/.^$*]/\\&/g')/,/$(printf '%s' "$END_MARKER" | sed 's/[][\\\/.^$*]/\\&/g')/p" "$README_PATH")
[ -n "$TABLE" ] || fail "could not extract the block between the deployed-address markers"

printf '%s\n' "$TABLE" | grep -qE '^\|[[:space:]]*Contract[[:space:]]*\|[[:space:]]*Address[[:space:]]*\|$' \
  || fail 'deployed-addresses table is missing the "| Contract | Address |" header row'

printf '%s\n' "$TABLE" | grep -qE '^\|[-:[:space:]]+\|[-:[:space:]]+\|?$' \
  || fail 'deployed-addresses table is missing the header/row separator row'

# Data rows: lines like "| registry_contract | `C...` |". The header starts
# with a capital "Contract" and the separator with dashes, so [a-z_] excludes
# both.
ROWS=$(printf '%s\n' "$TABLE" | grep -E '^\|[[:space:]]*[a-z_][a-z0-9_]*[[:space:]]*\|' || true)

if [ -z "$ROWS" ]; then
  fail 'deployed-addresses table has no data rows'
fi

row_count=$(printf '%s\n' "$ROWS" | grep -c . || true)
expected_count=$(printf '%s\n' $EXPECTED_CONTRACTS | grep -c .)
[ "$row_count" = "$expected_count" ] \
  || fail "expected $expected_count rows in the deployed-addresses table, found $row_count"

for name in $EXPECTED_CONTRACTS; do
  count=$(printf '%s\n' "$ROWS" | grep -cE "^\|[[:space:]]*${name}[[:space:]]*\|" || true)
  [ "$count" = "1" ] || fail "expected contract row '${name}' exactly once, found ${count} time(s)"
done

while IFS= read -r row; do
  name=$(printf '%s' "$row" | awk -F'|' '{gsub(/^[[:space:]]+|[[:space:]]+$/, "", $2); print $2}')
  addr=$(printf '%s' "$row" | awk -F'|' '{gsub(/^[[:space:]]+|[[:space:]]+$/, "", $3); print $3}')
  printf '%s\n' "$addr" | grep -qE "$ADDRESS_RE" \
    || fail "row '${name}': address ${addr} is not a Stellar contract ID (expected backtick-quoted C plus 55 base32 characters)"
done <<< "$ROWS"

echo "check-readme-addresses: OK - README deployed-addresses table is structurally valid (${row_count} rows: ${EXPECTED_CONTRACTS})"
