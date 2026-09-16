#!/usr/bin/env python3
"""Private macOS `security` protocol adapter for the bridge's temporary CLI cache.

Only our two Claude credential services are supported. This never accesses the
system keychain or the network. Durable storage remains in the parent redb DB.
"""
import hashlib
import json
import os
from pathlib import Path
import shlex
import sys
import tempfile


def main():
    directory = Path(os.environ['CLAUDE_SECURESTORAGE_CONFIG_DIR'])
    suffix = '-' + hashlib.sha256(str(directory).encode()).hexdigest()[:8]
    oauth_suffix = '-custom-oauth' if os.environ.get('CLAUDE_CODE_CUSTOM_OAUTH_URL') else ''
    services = {
        'Claude Code' + oauth_suffix + '-credentials' + suffix: directory / '.keychain-credentials.json',
        'Claude Code' + oauth_suffix + suffix: directory / '.console-key',
    }
    args = sys.argv[1:]
    if args == ['-i']:
        args = shlex.split(sys.stdin.read(262145))
    if args == ['show-keychain-info']:
        return 0
    if not args or '-s' not in args:
        return 44
    service = args[args.index('-s') + 1]
    destination = services.get(service)
    if destination is None or destination.is_symlink():
        return 44
    if args[0] == 'find-generic-password':
        if not destination.is_file():
            return 44
        sys.stdout.write(destination.read_text())
        return 0
    if args[0] == 'delete-generic-password':
        destination.unlink(missing_ok=True)
        return 0
    if args[0] != 'add-generic-password':
        return 1
    if '-X' in args:
        value = bytes.fromhex(args[args.index('-X') + 1]).decode()
    elif '-w' in args:
        value = args[args.index('-w') + 1]
    else:
        return 1
    if len(value.encode()) > 262144:
        return 1
    if destination.name == '.keychain-credentials.json':
        if not isinstance(json.loads(value), dict):
            return 1
    fd, temporary = tempfile.mkstemp(dir=directory, prefix='.credential-write-')
    try:
        with os.fdopen(fd, 'w') as output:
            output.write(value)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, destination)
    finally:
        Path(temporary).unlink(missing_ok=True)
    return 0


if __name__ == '__main__':
    try:
        result = main()
    except Exception:
        # Arguments and exceptions can contain tokens; never print them.
        result = 1
    sys.exit(result)
