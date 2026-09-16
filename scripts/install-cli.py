#!/usr/bin/env python3
"""Install a pinned official Linux CLI binary, verifying the release SHA256."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile
from urllib.request import urlopen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', default='2.1.272')
    parser.add_argument('--arch', choices=['amd64', 'arm64'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r'\d+\.\d+\.\d+', args.version):
        parser.error('version must be an exact x.y.z release')
    platform = 'linux-x64' if args.arch == 'amd64' else 'linux-arm64'
    base = f'https://downloads.claude.ai/claude-code-releases/{args.version}'
    with urlopen(base + '/manifest.json', timeout=60) as response:
        manifest = json.load(response)
    assert manifest['version'] == args.version, 'Release version mismatch'
    release = manifest['platforms'][platform]
    assert re.fullmatch(r'[a-f0-9]{64}', release['checksum']), 'Invalid release checksum'
    assert release['binary'] == 'claude', 'Unexpected binary name'
    args.output.parent.mkdir(parents=True, exist_ok=True)
    staging = None
    try:
        with tempfile.NamedTemporaryFile(dir=args.output.parent, delete=False) as output:
            staging = Path(output.name)
            digest = hashlib.sha256()
            size = 0
            with urlopen(f'{base}/{platform}/claude', timeout=120) as response:
                while chunk := response.read(1024 * 1024):
                    size += len(chunk)
                    if size > release['size']:
                        raise ValueError('Binary exceeds release size')
                    digest.update(chunk)
                    output.write(chunk)
        if size != release['size'] or digest.hexdigest() != release['checksum']:
            raise ValueError('CLI binary failed SHA256 or size verification')
        staging.chmod(0o755)
        os.replace(staging, args.output)
        print(f'Installed Claude Code {args.version} for {platform}; SHA256 verified.')
    finally:
        if staging is not None:
            staging.unlink(missing_ok=True)


if __name__ == '__main__':
    main()
