#!/usr/bin/env bash
# Storage migration fixture (#1207).
#
# Builds the contracts at a baseline git ref (what is deployed) and from the
# current checkout (the upgrade candidate), then runs tests/migration, which
# writes v1 state through the baseline Wasm, upgrades to the candidate Wasm and
# verifies representative registry, invoice, escrow and pool reads.
#
# Usage: bash scripts/migration_fixture.sh [--base-ref REF]
#   --base-ref REF   git commit, tag or branch deployed on-chain (default: HEAD~1)
#
# Requires: git, cargo, and the wasm32v1-none Rust target.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="wasm32v1-none"
PACKAGES=(trusttrove-registry trusttrove-invoice trusttrove-escrow trusttrove-pool)
WASMS=(trusttrove_registry trusttrove_invoice trusttrove_escrow trusttrove_pool)
BASE_REF="HEAD~1"

die() {
  echo "error: $*" >&2
  exit 1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --base-ref)
      [ "$#" -ge 2 ] || die "--base-ref needs a value"
      BASE_REF="$2"
      shift 2
      ;;
    -h | --help)
      sed -n '2,13p' "$0"
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

command -v git > /dev/null || die "git is required"
command -v cargo > /dev/null || die "cargo is required"
git -C "$ROOT" rev-parse --verify --quiet "${BASE_REF}^{commit}" > /dev/null \
  || die "base ref '${BASE_REF}' is not a commit in this checkout (fetch full history first)"

WORK="$ROOT/target/migration"
BASELINE_SRC="$WORK/baseline-src"
V1_DIR="$WORK/v1"
CURRENT_DIR="$WORK/current"

cleanup() {
  git -C "$ROOT" worktree remove --force "$BASELINE_SRC" > /dev/null 2>&1 || true
  git -C "$ROOT" worktree prune > /dev/null 2>&1 || true
}
trap cleanup EXIT

package_args() {
  local args=()
  for package in "${PACKAGES[@]}"; do
    args+=(-p "$package")
  done
  printf '%s\n' "${args[@]}"
}

collect() {
  local release_dir="$1" out_dir="$2"
  rm -rf "$out_dir"
  mkdir -p "$out_dir"
  for wasm in "${WASMS[@]}"; do
    [ -f "$release_dir/$wasm.wasm" ] || die "missing $release_dir/$wasm.wasm"
    cp "$release_dir/$wasm.wasm" "$out_dir/"
  done
}

mkdir -p "$WORK"
cleanup
echo "==> Baseline ref: $(git -C "$ROOT" rev-parse --short "${BASE_REF}^{commit}") (${BASE_REF})"
git -C "$ROOT" worktree add --detach "$BASELINE_SRC" "$BASE_REF" > /dev/null

mapfile -t PKG_ARGS < <(package_args)

echo "==> Building baseline Wasm"
(
  cd "$BASELINE_SRC"
  CARGO_TARGET_DIR="$WORK/baseline-target" \
    cargo build --release --target "$TARGET" "${PKG_ARGS[@]}"
)
collect "$WORK/baseline-target/$TARGET/release" "$V1_DIR"

echo "==> Building candidate Wasm"
(
  cd "$ROOT"
  cargo build --release --target "$TARGET" "${PKG_ARGS[@]}"
)
collect "$ROOT/target/$TARGET/release" "$CURRENT_DIR"

echo "==> Running storage migration fixture"
(
  cd "$ROOT"
  TRUSTTROVE_V1_WASM_DIR="$V1_DIR" \
    TRUSTTROVE_CURRENT_WASM_DIR="$CURRENT_DIR" \
    TRUSTTROVE_REQUIRE_MIGRATION=1 \
    cargo test -p trusttrove-migration-tests --test storage_migration -- --nocapture
)
echo "==> Storage migration fixture passed"
