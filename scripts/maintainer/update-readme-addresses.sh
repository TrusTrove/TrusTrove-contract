#!/bin/bash
set -euo pipefail

# This script updates the README.md with the currently deployed addresses
# by reading deployments.json. It relies on the injection markers:
# <!-- START_DEPLOYED_ADDRESSES -->
# <!-- END_DEPLOYED_ADDRESSES -->

REPO_ROOT="$(git rev-parse --show-toplevel)"
README_PATH="$REPO_ROOT/README.md"
DEPLOYMENTS_FILE="$REPO_ROOT/deployments.json"

if [ ! -f "$DEPLOYMENTS_FILE" ]; then
  echo "Error: $DEPLOYMENTS_FILE not found. Cannot update README.md."
  exit 1
fi

if [ ! -f "$README_PATH" ]; then
  echo "Error: $README_PATH not found."
  exit 1
fi

if ! command -v jq &> /dev/null; then
  echo "Error: jq is not installed. Please install jq to update README addresses."
  exit 1
fi

echo "Updating README.md with latest deployed addresses from deployments.json..."

# Read addresses using jq
registry=$(jq -r '.registry' "$DEPLOYMENTS_FILE")
invoice=$(jq -r '.invoice' "$DEPLOYMENTS_FILE")
escrow_usdc=$(jq -r '.escrow_usdc' "$DEPLOYMENTS_FILE")
pool_usdc=$(jq -r '.pool_usdc' "$DEPLOYMENTS_FILE")

# Prepare the new table content
NEW_TABLE="| Contract | Address |\n|----------|---------|\n"
NEW_TABLE+="| registry_contract | \`$registry\` |\n"
NEW_TABLE+="| invoice_contract | \`$invoice\` |\n"
NEW_TABLE+="| escrow_contract | \`$escrow_usdc\` |\n"
NEW_TABLE+="| pool_contract | \`$pool_usdc\` |\n"

# Use awk to replace the section between markers in README.md
awk -v new_content="$NEW_TABLE" '
    /<!-- START_DEPLOYED_ADDRESSES -->/ {
        print
        printf "%s", new_content
        skip = 1
        next
    }
    /<!-- END_DEPLOYED_ADDRESSES -->/ {
        skip = 0
    }
    !skip { print }
' "$README_PATH" > "${README_PATH}.tmp" && mv "${README_PATH}.tmp" "$README_PATH"

echo "README.md successfully updated with contract addresses."
