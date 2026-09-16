#!/usr/bin/env python3
"""Temporary local console server for browser tests, with no real account."""
import os
from pathlib import Path
import signal
import subprocess
import tempfile
ROOT=Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='bridge-console-test-') as directory:
    env={**os.environ,'BRIDGE_ADMIN_TOKEN':'browser-test-admin-token-minimum-32-bytes',
         'BRIDGE_API_KEY':'browser-messages-key','BRIDGE_CREDENTIAL_DB':directory+'/credentials.redb',
         'BRIDGE_BIND':'127.0.0.1:8791','BRIDGE_WEB_DIR':str(ROOT/'web/dist'),
         'BRIDGE_CLI_BARE':'0','RUST_LOG':'warn'}
    child=subprocess.Popen([str(ROOT/'target/debug/claude-messages-bridge')],env=env,cwd=ROOT)
    def stop(*_): child.terminate()
    signal.signal(signal.SIGTERM,stop);signal.signal(signal.SIGINT,stop)
    try: child.wait()
    finally:
        child.terminate()
        try: child.wait(timeout=5)
        except subprocess.TimeoutExpired: child.kill();child.wait()
