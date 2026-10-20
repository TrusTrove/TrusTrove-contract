#!/bin/bash
# load_env.sh — safe KEY=VALUE parser shared by deploy.sh and verify.sh.
#
# Loads an env file line by line and exports only well-formed KEY=VALUE
# pairs. Unlike `source`, this never executes the file as shell commands,
# so shell metacharacters or command substitutions in .env (which may be
# copy-pasted from testnet output or third-party contributions) cannot be
# used for code execution (see closed issue #455 and issue #612).
#
# Usage:
#   SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#   . "$SCRIPT_DIR/load_env.sh"
#   load_env .env

load_env() {
  local file="$1"
  if [ ! -f "$file" ]; then
    echo "Warning: $file not found; skipping." >&2
    return 1
  fi
  while IFS= read -r line || [ -n "$line" ]; do
    # Trim surrounding whitespace
    line="$(printf '%s' "$line" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
    # Skip blank lines and comments
    [ -z "$line" ] && continue
    case "$line" in
      \#*) continue ;;
    esac
    # Only accept KEY=VALUE pairs
    case "$line" in
      *=*)
        key="${line%%=*}"
        value="${line#*=}"
        # Trim trailing whitespace from the key
        key="$(printf '%s' "$key" | sed 's/[[:space:]]*$//')"
        # Strip matching surrounding quotes from the value
        case "$value" in
          \"*\") value="${value#\"}"; value="${value%\"}" ;;
          \'*\') value="${value#\'}"; value="${value%\'}" ;;
        esac
        # Only export keys that are valid shell identifiers; anything else
        # (e.g. keys with spaces or special characters) is skipped.
        case "$key" in
          *[!A-Za-z0-9_]*|'') continue ;;
        esac
        export "$key=$value"
        ;;
    esac
  done < "$file"
}
