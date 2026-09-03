#!/bin/bash
set -euo pipefail

# Thin wrapper around scripts/maintainer/create-contract-issues.sh
# Delegates to the canonical maintainer script which handles rate-limit
# retries and duplicate-issue guards.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/scripts/maintainer/create-contract-issues.sh" "$@"
