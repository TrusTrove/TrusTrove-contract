"""Write or verify the local WASM provenance for a testnet deployment."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess


WASM_DIR = Path('target/wasm32v1-none/release')
CONTRACTS = ('registry', 'invoice', 'escrow_usdc', 'escrow_xlm', 'pool_usdc', 'pool_xlm')
NETWORK_PASSPHRASE = 'Test SDF Network ; September 2015'


def checksums():
    return {
        path.as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
        for name in ('registry', 'invoice', 'escrow', 'pool')
        for path in [WASM_DIR / f'trusttrove_{name}.wasm']
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('write', 'verify'))
    parser.add_argument('manifest', type=Path)
    parser.add_argument('addresses', nargs='*')
    args = parser.parse_args()

    if args.action == 'verify':
        saved = json.loads(args.manifest.read_text(encoding='utf-8'))
        if saved.get('wasm_sha256') != checksums():
            parser.exit(1, 'WASM checksum verification failed\n')
        print('WASM checksums verified')
        return

    if len(args.addresses) != len(CONTRACTS) or not all(args.addresses):
        parser.error('expected one nonempty address per deployment target')
    manifest = dict(zip(CONTRACTS, args.addresses))
    manifest.update(
        updated_at=datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ'),
        network_passphrase=NETWORK_PASSPHRASE,
        source_commit=subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
        wasm_sha256=checksums(),
    )
    args.manifest.write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
