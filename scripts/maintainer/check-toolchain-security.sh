#!/bin/bash
set -euo pipefail

# Security and regression validation for rust-toolchain.toml resolution.
#
# Verifies that:
# 1. The repository's rust-toolchain.toml contains a valid, well-formed channel.
# 2. Malicious channel strings (command substitutions, shell metacharacters,
#    backticks, path traversals) are strictly rejected and never executed.
# 3. Legitimate toolchain patterns (stable, beta, nightly[-YYYY-MM-DD], semver)
#    are parsed successfully.
#
# Usage: scripts/maintainer/check-toolchain-security.sh [PATH_TO_RUST_TOOLCHAIN_TOML]

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
TARGET_FILE="${1:-$REPO_ROOT/rust-toolchain.toml}"

TOOLCHAIN_PATTERN='^(nightly(-[0-9]{4}-[0-9]{2}-[0-9]{2})?|beta(-[0-9]{4}-[0-9]{2}-[0-9]{2})?|stable|[0-9]+\.[0-9]+(\.[0-9]+)?)$'

resolve_toolchain() {
  local toml_file="$1"
  local channel
  channel=$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' "$toml_file")
  if [ -z "$channel" ]; then
    echo "ERROR: Could not read channel from $toml_file" >&2
    return 1
  fi
  if [[ ! "$channel" =~ $TOOLCHAIN_PATTERN ]]; then
    echo "ERROR: Invalid toolchain channel format: $channel" >&2
    return 1
  fi
  echo "$channel"
}

# 1. Check actual repo toolchain
echo "Validating repo toolchain file: $TARGET_FILE"
if [ ! -f "$TARGET_FILE" ]; then
  echo "ERROR: $TARGET_FILE not found" >&2
  exit 1
fi
RESOLVED=$(resolve_toolchain "$TARGET_FILE")
echo "OK: Repo toolchain resolved to valid channel: $RESOLVED"

# 2. Regression tests for malicious and invalid inputs
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

SENTINEL_FILE="$TMP_DIR/sentinel_executed"

MALICIOUS_CASES=(
  "\$(touch $SENTINEL_FILE)"
  "\`touch $SENTINEL_FILE\`"
  "1.85.0; touch $SENTINEL_FILE"
  "1.85.0 && touch $SENTINEL_FILE"
  "1.85.0 | touch $SENTINEL_FILE"
  "> $SENTINEL_FILE"
  "../../etc/passwd"
  "rm -rf /"
  "1.85.0\ntouch $SENTINEL_FILE"
  "malicious-channel-name"
  ""
)

for payload in "${MALICIOUS_CASES[@]}"; do
  test_file="$TMP_DIR/test-toolchain.toml"
  cat > "$test_file" <<EOF
[toolchain]
channel = "$payload"
EOF
  if resolve_toolchain "$test_file" >/dev/null 2>&1; then
    echo "SECURITY FAILURE: Malicious payload '$payload' was unexpectedly accepted!" >&2
    exit 1
  fi
  if [ -f "$SENTINEL_FILE" ]; then
    echo "SECURITY FAILURE: Command execution detected from payload '$payload'!" >&2
    rm -f "$SENTINEL_FILE"
    exit 1
  fi
done
echo "OK: All malicious payloads rejected without command execution."

# 3. Test valid channel variations
VALID_CASES=(
  "1.85.0"
  "1.85"
  "stable"
  "beta"
  "nightly"
  "nightly-2024-05-01"
  "beta-2024-05-01"
)

for valid_channel in "${VALID_CASES[@]}"; do
  test_file="$TMP_DIR/test-toolchain.toml"
  cat > "$test_file" <<EOF
[toolchain]
channel = "$valid_channel"
EOF
  res=$(resolve_toolchain "$test_file")
  if [ "$res" != "$valid_channel" ]; then
    echo "ERROR: Valid channel '$valid_channel' did not resolve correctly (got '$res')" >&2
    exit 1
  fi
done
echo "OK: All expected toolchain variations parsed successfully."

echo "check-toolchain-security: OK - toolchain resolution is secure and regression verified."
