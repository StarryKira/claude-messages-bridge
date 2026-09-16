#!/usr/bin/env python3
"""Create local configuration without displaying secrets or replacing existing files."""
import os
from pathlib import Path
import secrets

ROOT = Path(__file__).resolve().parents[1]
destination = ROOT / ".env"
runtime = ROOT / ".bridge"
runtime.mkdir(mode=0o700, exist_ok=True)
runtime.chmod(0o700)
try:
    descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
except FileExistsError:
    print(f"Existing configuration preserved: {destination}")
else:
    with os.fdopen(descriptor, "w") as output:
        output.write(
            "# Local configuration. Keep this file private.\n"
            "BRIDGE_BIND=127.0.0.1:8787\n"
            f"BRIDGE_ADMIN_TOKEN={secrets.token_urlsafe(32)}\n"
            f"BRIDGE_API_KEY={secrets.token_urlsafe(32)}\n"
            "BRIDGE_CREDENTIAL_DB=.bridge/credentials.redb\n"
            "BRIDGE_WEB_DIR=web/dist\n"
            "BRIDGE_CLI_BARE=0\n"
        )
    print(f"Created {destination} (mode 0600); secrets were not printed.")
