#!/bin/bash

# Exit immediately if any command in a pipeline fails, or if a variable is unset
set -u

# CLI path configuration (matching deploy.sh with fallback)
STELLAR="/mnt/c/Program Files (x86)/Stellar CLI/stellar.exe"
if [ ! -f "$STELLAR" ]; then
  STELLAR="stellar"
fi

# Load environment configuration (first checking .env, then falling back to .env.example)
if [ -f .env ]; then
  source .env
else
  source .env.example
fi

# Tracking execution status
FAILED=0

# Helper function to assert that required variables are set
check_env_var() {
  local var_name="$1"
  # Use indirect expansion to get value of var_name
  eval var_val=\$$var_name
  if [ -z "$var_val" ]; then
    echo "Error: $var_name is not set or is empty."
    FAILED=1
  fi
}

echo "=== Verifying Contract Deployment Configuration ==="
check_env_var "DEPLOYER_ACCOUNT"
check_env_var "REGISTRY_CONTRACT_ID"
check_env_var "INVOICE_CONTRACT_ID"
check_env_var "POOL_USDC_CONTRACT_ID"
check_env_var "POOL_XLM_CONTRACT_ID"
check_env_var "ESCROW_USDC_CONTRACT_ID"
check_env_var "ESCROW_XLM_CONTRACT_ID"

if [ $FAILED -ne 0 ]; then
  echo "Configuration check failed. Please ensure environment variables are populated."
  exit 1
fi

# Helper function to run a check and report status
verify_check() {
  local name="$1"
  local cmd="$2"
  
  echo "Verifying $name..."
  local output
  output=$(eval "$cmd" 2>&1)
  local status=$?
  
  if [ $status -eq 0 ]; then
    echo "  [PASS] $name"
    echo "  Result: $output"
  else
    echo "  [FAIL] $name"
    echo "  Error: $output"
    FAILED=1
  fi
  echo ""
}

echo "=== Running Contract Verification Queries ==="

# 1. Registry Contract - get_admin
CMD_REGISTRY="\"\$STELLAR\" contract invoke --id \"\$REGISTRY_CONTRACT_ID\" --source \"\$DEPLOYER_ACCOUNT\" --network testnet -- get_admin"
verify_check "registry_contract (get_admin)" "$CMD_REGISTRY"

# 2. Invoice Contract - get_counts
CMD_INVOICE="\"\$STELLAR\" contract invoke --id \"\$INVOICE_CONTRACT_ID\" --source \"\$DEPLOYER_ACCOUNT\" --network testnet -- get_counts"
verify_check "invoice_contract (get_counts)" "$CMD_INVOICE"

# 3. USDC Pool Contract - get_stats
CMD_POOL_USDC="\"\$STELLAR\" contract invoke --id \"\$POOL_USDC_CONTRACT_ID\" --source \"\$DEPLOYER_ACCOUNT\" --network testnet -- get_stats"
verify_check "pool_usdc_contract (get_stats)" "$CMD_POOL_USDC"

# 4. XLM Pool Contract - get_stats
CMD_POOL_XLM="\"\$STELLAR\" contract invoke --id \"\$POOL_XLM_CONTRACT_ID\" --source \"\$DEPLOYER_ACCOUNT\" --network testnet -- get_stats"
verify_check "pool_xlm_contract (get_stats)" "$CMD_POOL_XLM"

# 5. USDC Escrow Contract - get_locked (confirm existence with dummy ID)
CMD_ESCROW_USDC="\"\$STELLAR\" contract invoke --id \"\$ESCROW_USDC_CONTRACT_ID\" --source \"\$DEPLOYER_ACCOUNT\" --network testnet -- get_locked --invoice_id 0000000000000000000000000000000000000000000000000000000000000000"
verify_check "escrow_usdc_contract (get_locked)" "$CMD_ESCROW_USDC"

# 6. XLM Escrow Contract - get_locked (confirm existence with dummy ID)
CMD_ESCROW_XLM="\"\$STELLAR\" contract invoke --id \"\$ESCROW_XLM_CONTRACT_ID\" --source \"\$DEPLOYER_ACCOUNT\" --network testnet -- get_locked --invoice_id 0000000000000000000000000000000000000000000000000000000000000000"
verify_check "escrow_xlm_contract (get_locked)" "$CMD_ESCROW_XLM"

if [ $FAILED -ne 0 ]; then
  echo "Verification failed."
  exit 1
else
  echo "All contract verifications passed successfully."
  exit 0
fi
