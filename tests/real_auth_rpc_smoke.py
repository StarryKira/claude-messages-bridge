#!/usr/bin/env python3
"""Real CLI account RPC + local TLS OAuth/API fixture; never contacts Anthropic.

The CLI is unmodified. HTTPS_PROXY points at a loopback fixture, which terminates
CONNECT locally and never forwards traffic. Only its temporary CA is trusted.
"""
import argparse
import http.server
import json
import os
import re
from pathlib import Path
import signal
import socket
import ssl
import subprocess
import tempfile
import threading
import time
from urllib.parse import urlsplit, parse_qs
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument('--cli', required=True)
parser.add_argument('--bridge', type=Path, default=ROOT/'target/debug/claude-messages-bridge')
args = parser.parse_args()
seen = []
access = 'sk-ant-oat01-local-rpc-access'
refreshed = 'sk-ant-oat01-local-rpc-refreshed'
console_key = 'sk-ant-api03-local-rpc-console-key'
account_id = '11111111-1111-4111-8111-111111111111'
other_id = '33333333-3333-4333-8333-333333333333'
other_access = 'sk-ant-oat01-local-rpc-other'


def response_events():
    return [
        {'type':'message_start','message':{'id':'msg_native_rpc','type':'message','role':'assistant','model':'claude-sonnet-4-6','content':[],'stop_reason':None,'stop_sequence':None,'usage':{'input_tokens':4,'output_tokens':0}}},
        {'type':'content_block_start','index':0,'content_block':{'type':'text','text':''}},
        {'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':'native OAuth RPC works'}},
        {'type':'content_block_stop','index':0},
        {'type':'message_delta','delta':{'stop_reason':'end_turn','stop_sequence':None},'usage':{'output_tokens':5}},
        {'type':'message_stop'},
    ]


with tempfile.TemporaryDirectory(prefix='bridge-real-auth-rpc-') as directory:
    root = Path(directory)
    cert, key = root/'ca.pem', root/'ca.key'
    subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-keyout',str(key),'-out',str(cert),'-days','1','-subj','/CN=Local bridge test', '-addext','subjectAltName=DNS:platform.claude.com,DNS:api.anthropic.com,DNS:claude.com,DNS:claude.ai,DNS:console.anthropic.com'],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(cert,key)

    class Fixture(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'
        def log_message(self, *_): pass
        def do_CONNECT(self):
            self.send_response(200);self.end_headers();self.wfile.flush()
            self.connection=tls.wrap_socket(self.connection,server_side=True)
            self.rfile=self.connection.makefile('rb')
            self.wfile=self.connection.makefile('wb')
            self.close_connection=False
        def reply(self,value,status=200,kind='application/json'):
            body=value if isinstance(value,bytes) else json.dumps(value).encode()
            self.send_response(status);self.send_header('Content-Type',kind);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body);self.wfile.flush()
        def do_GET(self): self.handle_api()
        def do_POST(self): self.handle_api()
        def handle_api(self):
            raw=self.rfile.read(int(self.headers.get('Content-Length','0')))
            payload=json.loads(raw) if raw else None
            path=urlsplit(self.path).path
            seen.append((path,payload,dict(self.headers)))
            if path=='/v1/oauth/token':
                is_refresh=payload['grant_type']=='refresh_token'
                if not is_refresh:
                    assert payload['code_verifier'] and payload['state']
                    assert payload['code'] in ('ok','console','other')
                self.reply({'access_token':refreshed if is_refresh else other_access if payload.get('code')=='other' else access,'refresh_token':'dummy-rotated-refresh' if is_refresh else 'dummy-cli-refresh','expires_in':3600 if is_refresh else 1,'scope':'org:create_api_key user:profile' if payload.get('code')=='console' else 'user:inference user:profile','account':{'uuid':other_id if payload.get('code')=='other' else account_id,'email_address':'rpc-test@example.com'},'organization':{'uuid':'22222222-2222-4222-8222-222222222222'}})
            elif path=='/api/oauth/profile':
                self.reply({'account':{'uuid':other_id if self.headers.get('Authorization')=='Bearer '+other_access else account_id,'email':'rpc-test@example.com','display_name':'RPC Test','created_at':'2025-01-01T00:00:00Z'},'organization':{'uuid':'22222222-2222-4222-8222-222222222222','name':'Local Fixture','organization_type':'claude_pro','rate_limit_tier':'default_claude_pro','billing_type':'stripe','has_extra_usage_enabled':False,'subscription_created_at':'2025-01-01T00:00:00Z'}})
            elif path=='/api/oauth/claude_cli/create_api_key': self.reply({'raw_key':console_key})
            elif path=='/api/oauth/claude_cli/roles': self.reply({'organization_role':'admin','workspace_role':'admin'})
            elif path=='/v1/messages':
                lowered={k.lower():v for k,v in self.headers.items()}
                assert lowered.get('authorization') in ('Bearer '+access,'Bearer '+refreshed) or lowered.get('x-api-key')==console_key
                self.reply(b''.join(('event: '+v['type']+'\ndata: '+json.dumps(v)+'\n\n').encode() for v in response_events()),kind='text/event-stream')
            elif 'first_token' in path: self.reply({'first_token_date':None})
            else: self.reply({})

    proxy=http.server.ThreadingHTTPServer(('127.0.0.1',0),Fixture)
    proxy.daemon_threads=True
    threading.Thread(target=proxy.serve_forever,daemon=True).start()
    with socket.socket() as reserve:
        reserve.bind(('127.0.0.1',0)); port=reserve.getsockname()[1]
    base=f'http://127.0.0.1:{port}'
    token='local-admin-token-for-native-rpc-test'
    env={k:v for k,v in os.environ.items() if k in ('PATH','TMPDIR','LANG','USER')}
    env.update(HOME=str(root),CLAUDE_CLI_PATH=str(Path(args.cli).expanduser().absolute()),BRIDGE_BIND=f'127.0.0.1:{port}',BRIDGE_ADMIN_TOKEN=token,BRIDGE_API_KEY='local-api-key',BRIDGE_CREDENTIAL_DB=str(root/'credentials.redb'),BRIDGE_INIT_TIMEOUT_SECONDS='20',BRIDGE_TIMEOUT_SECONDS='40',BRIDGE_OAUTH_TIMEOUT_SECONDS='60',HTTPS_PROXY=f'http://127.0.0.1:{proxy.server_port}',HTTP_PROXY=f'http://127.0.0.1:{proxy.server_port}',NO_PROXY='localhost,127.0.0.1',NODE_EXTRA_CA_CERTS=str(cert),SSL_CERT_FILE=str(cert),RUST_LOG='warn')
    child=None
    def api(path,body=None,method=None):
        request=Request(base+path,data=json.dumps(body).encode() if body is not None else None,method=method,headers={'Authorization':'Bearer '+token,'Content-Type':'application/json','x-api-key':'local-api-key'})
        with urlopen(request,timeout=45) as response:return json.load(response)
    def launch():
        global child
        child=subprocess.Popen([str(args.bridge.resolve())],cwd=ROOT,env=env,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        deadline=time.monotonic()+10
        while time.monotonic()<deadline:
            if child.poll() is not None:raise AssertionError('Bridge exited during startup')
            try:api('/healthz');return
            except OSError:time.sleep(.05)
        raise AssertionError('Bridge startup timed out')
    def stop():
        child.send_signal(signal.SIGTERM)
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
        child.stderr.close()
    def login(method,code,expected="succeeded"):
        session=api('/api/admin/oauth/start',{'method':method})
        parsed=urlsplit(session['authorization_url']);query=parse_qs(parsed.query)
        assert query['code_challenge_method']==['S256']
        api(f"/api/admin/oauth/{session['id']}/code",{'code':code+'#'+query['state'][0]})
        deadline=time.monotonic()+40
        while time.monotonic()<deadline:
            current=api('/api/admin/oauth')['login']
            if current['status'] not in ('waiting','submitting','starting'):
                assert current['status']==expected,current
                break
            time.sleep(.1)
        else:raise AssertionError('Login timed out')
        # Completion is published after the CLI's native writes. Wait for child reaping.
        while api('/api/admin/status')['service']['available_slots']==0:time.sleep(.05)
        if expected=='succeeded':assert api('/api/admin/status')['account']['logged_in']
        else:assert 'bound to another account' in current['message']
    def message(system="RPC_SYSTEM_A", history=None):
        result=api('/v1/messages',{'model':'claude-sonnet-4-6','max_tokens':128,'system':system,'messages':history or [{'role':'user','content':'hello'}]})
        assert result['content'][0]['text']=='native OAuth RPC works'
        while api('/api/admin/status')['service']['available_slots']<4:time.sleep(.05)
        return next(payload for path,payload,_ in reversed(seen) if path=='/v1/messages')
    def prompt_check(upstream, expected):
        texts=[block['text'] for block in upstream['system'] if block['type']=='text']
        headers=[text for text in texts if text.startswith('x-anthropic-billing-header:')]
        identities=[text for text in texts if text.startswith('You are Claude Code,') or text.startswith('You are a Claude agent,')]
        assert len(headers)==1 and len(identities)==1, texts
        assert re.search(r'cch=[0-9a-f]{5};',headers[0]),headers
        assert 'cch=00000;' not in headers[0] and 'cc_version=0.0.0.' not in headers[0], headers
        assert expected in texts, texts
        return [{'type':'text','text':text} for text in texts]

    try:
        launch();login('claudeai','ok');print('PASS real CLI native authenticate/callback RPC, PKCE, profile and redb save',flush=True)
        assert api('/api/admin/status')['account']['binding']['account_id']==account_id
        login('claudeai','other',expected='failed')
        upstream=message()
        wrapped=prompt_check(upstream,'RPC_SYSTEM_A')
        # Replay a caller-supplied CLI wrapper with a changed body and stale cch.
        for block in wrapped:
            if block['text'].startswith('x-anthropic-billing-header:'):
                block['text']='x-anthropic-billing-header: cc_version=0.0.0.abc; cc_entrypoint=sdk-cli; cch=00000;'
            elif block['text']=='RPC_SYSTEM_A':block['text']='RPC_SYSTEM_B'
        history=[{'role':'user','content':'first turn'},{'role':'assistant','content':'earlier answer'},{'role':'user','content':'next turn'}]
        changed=message(wrapped,history)
        prompt_check(changed,'RPC_SYSTEM_B')
        assert all('RPC_SYSTEM_A' not in block.get('text','') for block in changed['system'])
        print('PASS changed system prompt, native prefix/cch injection and stale wrapper removal',flush=True)
        refreshes=sum(path=='/v1/oauth/token' and payload.get('grant_type')=='refresh_token' for path,payload,_ in seen)
        assert refreshes>=1
        print('PASS real CLI native refresh and Messages inference',flush=True)
        stop();launch();message()
        assert sum(path=='/v1/oauth/token' and payload.get('grant_type')=='refresh_token' for path,payload,_ in seen)==refreshes
        print('PASS redb restart restores rotated native credentials',flush=True)
        api('/api/admin/logout',{})
        stop();launch()
        assert api('/api/admin/status')['account']['binding']['account_id']==account_id
        login('claudeai','other',expected='failed')
        assert not api('/api/admin/status')['account']['logged_in']
        print('PASS native account UUID binding survives logout/restart and rejects other accounts',flush=True)
        login('console','console');message()
        assert any(path=='/api/oauth/claude_cli/create_api_key' for path,_,_ in seen)
        print('PASS Console account RPC, native API key creation and inference',flush=True)
        api('/api/admin/logout',{})
        assert not api('/api/admin/status')['account']['logged_in']
        print(f'All native auth RPC checks passed; {len(seen)} local fixture requests, zero forwarded requests.',flush=True)
    except Exception:
        print('Fixture paths:',[path for path,_,_ in seen],flush=True)
        raise
    finally:
        if child and child.poll() is None:stop()
        proxy.shutdown();proxy.server_close()
