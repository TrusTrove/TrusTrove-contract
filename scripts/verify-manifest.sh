#!/usr/bin/env bash
set -euo pipefail

MANIFEST_PATH="${1:-deployment-manifest.json}"

if [ ! -f "$MANIFEST_PATH" ]; then
  echo "Error: Deployment manifest not found at $MANIFEST_PATH" >&2
  exit 1
fi

echo "Verifying deployment manifest checksums and metadata against $MANIFEST_PATH ..."

# Use python to parse json safely and recompute/check hashes
python3 - <<'EOF' "$MANIFEST_PATH"
import json, hashlib, os, sys

manifest_path = sys.argv[1] if len(sys.argv) > 1 else "deployment-manifest.json"
with open(manifest_path, "r") as f:
    manifest = json.load(f)

wasm_dir = "target/wasm32v1-none/release"
failed = False

for filename, expected_hash in manifest.get("wasm_checksums", {}).items():
    if not expected_hash:
        print(f"Warning: No expected checksum recorded for {filename} in manifest.")
        continue
    wasm_path = os.path.join(wasm_dir, filename)
    if not os.path.exists(wasm_path):
        print(f"Error: WASM file {wasm_path} does not exist.")
        failed = True
        continue
    
    sha = hashlib.sha256()
    with open(wasm_path, "rb") as wf:
        while chunk := wf.read(65536):
            sha.update(chunk)
    actual_hash = sha.hexdigest()
    
    if actual_hash != expected_hash:
        print(f"MISMATCH: {filename} hash is {actual_hash}, expected {expected_hash}")
        failed = True
    else:
        print(f"MATCH: {filename} ({actual_hash})")

if failed:
    print("Verification failed: WASM checksums do not match manifest.")
    sys.exit(1)
else:
    print("All deployment manifest WASM checksums verified successfully.")
EOF
