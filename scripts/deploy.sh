#!/bin/bash
set -e

STELLAR="/mnt/c/Program Files (x86)/Stellar CLI/stellar.exe"

if [ -f .env ]; then
  source .env
else
  source .env.example
fi

echo "=== Building all contracts ==="
"$STELLAR" contract build

echo ""
echo "=== Deploying registry_contract ==="
REGISTRY_ID=$("$STELLAR" contract deploy \
  --wasm target/wasm32v1-none/release/trusttrove_registry.wasm \
  --source $DEPLOYER_ACCOUNT \
  --network testnet)
echo "Registry: $REGISTRY_ID"
sleep 3

"$STELLAR" contract invoke \
  --id $REGISTRY_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- initialize \
  --admin $("$STELLAR" keys address $DEPLOYER_ACCOUNT)
sleep 3

echo ""
echo "=== Deploying invoice_contract ==="
INVOICE_ID=$("$STELLAR" contract deploy \
  --wasm target/wasm32v1-none/release/trusttrove_invoice.wasm \
  --source $DEPLOYER_ACCOUNT \
  --network testnet)
echo "Invoice: $INVOICE_ID"
sleep 3

"$STELLAR" contract invoke \
  --id $INVOICE_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- initialize \
  --admin $("$STELLAR" keys address $DEPLOYER_ACCOUNT) \
  --registry_contract $REGISTRY_ID
sleep 3

echo ""
echo "=== Deploying USDC escrow_contract ==="
ESCROW_USDC_ID=$("$STELLAR" contract deploy \
  --wasm target/wasm32v1-none/release/trusttrove_escrow.wasm \
  --source $DEPLOYER_ACCOUNT \
  --network testnet)
echo "USDC Escrow: $ESCROW_USDC_ID"
sleep 3

echo ""
echo "=== Deploying USDC pool_contract ==="
POOL_USDC_ID=$("$STELLAR" contract deploy \
  --wasm target/wasm32v1-none/release/trusttrove_pool.wasm \
  --source $DEPLOYER_ACCOUNT \
  --network testnet)
echo "USDC Pool: $POOL_USDC_ID"
sleep 3

echo ""
echo "=== Initializing USDC escrow ==="
"$STELLAR" contract invoke \
  --id $ESCROW_USDC_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- initialize \
  --admin $("$STELLAR" keys address $DEPLOYER_ACCOUNT) \
  --pool_contract $POOL_USDC_ID \
  --invoice_contract $INVOICE_ID \
  --usdc_asset $USDC_ISSUER
sleep 3

echo ""
echo "=== Initializing USDC pool ==="
"$STELLAR" contract invoke \
  --id $POOL_USDC_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- initialize \
  --admin $("$STELLAR" keys address $DEPLOYER_ACCOUNT) \
  --invoice_contract $INVOICE_ID \
  --escrow_contract $ESCROW_USDC_ID \
  --usdc_asset $USDC_ISSUER
sleep 3

echo ""
echo "=== Deploying XLM escrow_contract ==="
ESCROW_XLM_ID=$("$STELLAR" contract deploy \
  --wasm target/wasm32v1-none/release/trusttrove_escrow.wasm \
  --source $DEPLOYER_ACCOUNT \
  --network testnet)
echo "XLM Escrow: $ESCROW_XLM_ID"
sleep 3

echo ""
echo "=== Deploying XLM pool_contract ==="
POOL_XLM_ID=$("$STELLAR" contract deploy \
  --wasm target/wasm32v1-none/release/trusttrove_pool.wasm \
  --source $DEPLOYER_ACCOUNT \
  --network testnet)
echo "XLM Pool: $POOL_XLM_ID"
sleep 3

echo ""
echo "=== Initializing XLM escrow ==="
"$STELLAR" contract invoke \
  --id $ESCROW_XLM_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- initialize \
  --admin $("$STELLAR" keys address $DEPLOYER_ACCOUNT) \
  --pool_contract $POOL_XLM_ID \
  --invoice_contract $INVOICE_ID \
  --usdc_asset $XLM_ASSET
sleep 3

echo ""
echo "=== Initializing XLM pool ==="
"$STELLAR" contract invoke \
  --id $POOL_XLM_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- initialize \
  --admin $("$STELLAR" keys address $DEPLOYER_ACCOUNT) \
  --invoice_contract $INVOICE_ID \
  --escrow_contract $ESCROW_XLM_ID \
  --usdc_asset $XLM_ASSET
sleep 3

echo ""
echo "=== Wiring USDC pool_contract into invoice_contract ==="
"$STELLAR" contract invoke \
  --id $INVOICE_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- set_pool_contract \
  --pool_contract $POOL_USDC_ID
sleep 3

echo ""
echo "==========================================="
echo "Deployment complete. Add to trusttrove-app .env.local:"
echo ""
echo "NEXT_PUBLIC_REGISTRY_CONTRACT_ID=$REGISTRY_ID"
echo "NEXT_PUBLIC_INVOICE_CONTRACT_ID=$INVOICE_ID"
echo "NEXT_PUBLIC_ESCROW_USDC_CONTRACT_ID=$ESCROW_USDC_ID"
echo "NEXT_PUBLIC_ESCROW_XLM_CONTRACT_ID=$ESCROW_XLM_ID"
echo "NEXT_PUBLIC_POOL_USDC_CONTRACT_ID=$POOL_USDC_ID"
echo "NEXT_PUBLIC_POOL_XLM_CONTRACT_ID=$POOL_XLM_ID"
echo "==========================================="
