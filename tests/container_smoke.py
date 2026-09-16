#!/usr/bin/env python3
"""Exercise the built image offline, including real CLI RPC and persistent redb."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time
from urllib.error import HTTPError
from urllib.parse import parse_qs, urlsplit
from urllib.request import Request, urlopen
import uuid

ADMIN = 'container-test-admin-token-at-least-32-bytes'
KEY = 'container-test-messages-key'


def api(path, body=None, token=ADMIN):
    request = Request('http://127.0.0.1:8787' + path,
                      data=None if body is None else json.dumps(body).encode(),
                      headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
    with urlopen(request, timeout=10) as response:
        return json.load(response)


def login(code):
    session = api('/api/admin/oauth/start', {})
    state = parse_qs(urlsplit(session['authorization_url']).query)['state'][0]
    api(f"/api/admin/oauth/{session['id']}/code", {'code': code + '#' + state})
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        result = api('/api/admin/oauth')['login']
        if result['status'] not in ('waiting', 'starting', 'submitting'):
            if api('/api/admin/status')['service']['available_slots'] == 4:
                return result
        time.sleep(.05)
    raise AssertionError('Container login timed out')


def inside(phase):
    assert os.getuid() == 10001, 'Container must run without root'
    deadline = time.monotonic() + 15
    while True:
        try:
            status = api('/api/admin/status')
            break
        except OSError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(.1)
    assert (Path('/data/credentials.redb').stat().st_mode & 0o777) == 0o600
    try:
        api('/api/admin/status', token=KEY)
    except HTTPError as error:
        assert error.code == 401
    else:
        raise AssertionError('Messages key unlocked administration')
    with urlopen('http://127.0.0.1:8787/admin/', timeout=5) as response:
        assert 'assets/index-' in response.read().decode()
    if phase == 'first':
        assert status['account']['binding'] is None
        assert login('ok')['status'] == 'succeeded'
    else:
        assert status['account']['logged_in']
        assert status['account']['binding']['account_id'] == 'fixture-id'
        api('/api/admin/logout', {})
        assert login('other')['status'] == 'failed'
        status = api('/api/admin/status')['account']
        assert not status['logged_in'] and status['binding']['account_id'] == 'fixture-id'
    print(f'PASS container {phase}: non-root, admin auth, React assets and account persistence', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image')
    parser.add_argument('--inside', choices=['first', 'restart'])
    args = parser.parse_args()
    if args.inside:
        inside(args.inside)
        return
    if not args.image:
        parser.error('--image is required outside the container')
    name = 'bridge-check-' + uuid.uuid4().hex[:12]
    volume = name + '-data'
    tests = Path(__file__).resolve().parent

    def docker(*command, **kwargs):
        return subprocess.run(['docker', *command], check=True, timeout=300, **kwargs)

    try:
        docker('volume', 'create', volume, stdout=subprocess.DEVNULL)
        docker('run', '-d', '--name', name, '--network', 'none',
               '--mount', f'type=volume,source={volume},target=/data',
               '--mount', f'type=bind,source={tests},target=/checks,readonly',
               '-e', 'BRIDGE_ADMIN_TOKEN=' + ADMIN, '-e', 'BRIDGE_API_KEY=' + KEY,
               '-e', 'CLAUDE_CLI_PATH=/checks/fixtures/cli.py', args.image,
               stdout=subprocess.DEVNULL)
        docker('exec', name, 'python3', '/checks/container_smoke.py', '--inside', 'first')
        docker('exec', name, 'python3', '/app/healthcheck.py')
        docker('restart', name, stdout=subprocess.DEVNULL)
        docker('exec', name, 'python3', '/checks/container_smoke.py', '--inside', 'restart')
        # The network namespace has only loopback: test fixtures cannot reach the provider.
        for script in ['real_auth_rpc_smoke.py', 'real_cli_smoke.py']:
            docker('exec', name, 'python3', '/checks/' + script,
                   '--cli', '/usr/local/bin/claude', '--bridge', '/usr/local/bin/claude-messages-bridge')
        print('All container checks passed with external networking disabled.', flush=True)
    finally:
        subprocess.run(['docker', 'rm', '-f', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        subprocess.run(['docker', 'volume', 'rm', volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


if __name__ == '__main__':
    main()
