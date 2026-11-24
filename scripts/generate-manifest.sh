#!/usr/bin/env bash
set -euo pipefail

MANIFEST_PATH="${1:-deployment-manifest.json}"
NETWORK_PASSPHRASE="${NETWORK_PASSPHRASE:-Test SDF Network ; September 2015}"
SOURCE_COMMIT="$(git rev-parse HEAD 2>/dev/null || echo "unknown")"

echo "Generating deployment manifest at $MANIFEST_PATH ..."

# Collect WASM checksums
WASM_DIR="target/wasm32v1-none/release"
registry_sha=""
invoice_sha=""
escrow_sha=""
pool_sha=""
pool_factory_sha=""

if [ -f "$WASM_DIR/trusttrove_registry.wasm" ]; then
  registry_sha="$(sha256sum "$WASM_DIR/trusttrove_registry.wasm" | awk '{print $1}')"
fi
if [ -f "$WASM_DIR/trusttrove_invoice.wasm" ]; then
  invoice_sha="$(sha256sum "$WASM_DIR/trusttrove_invoice.wasm" | awk '{print $1}')"
fi
if [ -f "$WASM_DIR/trusttrove_escrow.wasm" ]; then
  escrow_sha="$(sha256sum "$WASM_DIR/trusttrove_escrow.wasm" | awk '{print $1}')"
fi
if [ -f "$WASM_DIR/trusttrove_pool.wasm" ]; then
  pool_sha="$(sha256sum "$WASM_DIR/trusttrove_pool.wasm" | awk '{print $1}')"
fi
if [ -f "$WASM_DIR/trusttrove_pool_factory.wasm" ]; then
  pool_factory_sha="$(sha256sum "$WASM_DIR/trusttrove_pool_factory.wasm" | awk '{print $1}')"
fi

# Load contract IDs from .deployed-addresses if available
registry_id=""
invoice_id=""
escrow_usdc_id=""
pool_usdc_id=""
if [ -f ".deployed-addresses" ]; then
  source .deployed-addresses
  registry_id="${REGISTRY_ID:-${registry_id:-}}"
  invoice_id="${INVOICE_ID:-${invoice_id:-}}"
  escrow_usdc_id="${ESCROW_USDC_ID:-${escrow_usdc_id:-}}"
  pool_usdc_id="${POOL_USDC_ID:-${pool_usdc_id:-}}"
fi

cat <<EOF > "$MANIFEST_PATH"
{
  "source_commit": "$SOURCE_COMMIT",
  "network_passphrase": "$NETWORK_PASSPHRASE",
  "contract_ids": {
    "registry": "$registry_id",
    "invoice": "$invoice_id",
    "escrow_usdc": "$escrow_usdc_id",
    "pool_usdc": "$pool_usdc_id"
  },
  "wasm_checksums": {
    "trusttrove_registry.wasm": "$registry_sha",
    "trusttrove_invoice.wasm": "$invoice_sha",
    "trusttrove_escrow.wasm": "$escrow_sha",
    "trusttrove_pool.wasm": "$pool_sha",
    "trusttrove_pool_factory.wasm": "$pool_factory_sha"
  }
}
EOF

echo "Deployment manifest generated successfully at $MANIFEST_PATH."
