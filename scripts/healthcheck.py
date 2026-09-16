#!/usr/bin/env python3
import json
import os
from urllib.request import ProxyHandler, build_opener

bind = os.environ.get('BRIDGE_BIND', '0.0.0.0:8787')
port = int(bind.rsplit(':', 1)[1])
host = '[::1]' if bind.startswith('[') else '127.0.0.1'
with build_opener(ProxyHandler({})).open(f'http://{host}:{port}/healthz', timeout=3) as response:
    if json.load(response).get('status') != 'ok':
        raise SystemExit(1)
