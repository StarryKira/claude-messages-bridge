#!/usr/bin/env python3
"""Real CLI + local fake Anthropic API integration. No real credentials/model calls.
Run: python3 tests/real_cli_smoke.py --cli /absolute/path/to/claude
"""
import argparse
import json
import os
import re
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]
CAPTURED = []
LOCK = threading.Lock()

def available_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]

def content(messages):
    result = []
    for m in messages:
        c = m['content']
        result.extend(c if isinstance(c, list) else [{'type':'text','text':c}])
    return result

class FakeApi(BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(b'{}')
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('content-length', 0))) or '{}')
        if '/messages' not in self.path:
            self.send_response(200); self.end_headers(); self.wfile.write(b'{}'); return
        with LOCK: CAPTURED.append(body)
        blocks = content(body['messages'][-1:])
        tool_results = [b for b in blocks if b['type'] == 'tool_result']
        text = ''.join(b.get('text','') for b in blocks)
        use_tools = 'USE_TOOL' in text and not tool_results
        if use_tools:
            tools = body.get('tools', [])
            name = next(t['name'] for t in tools if t['name'].endswith('__weather'))
            generated = [{'type':'tool_use','id':f'toolu_{i}','name':name,'input':{'city':city}}
                         for i,city in enumerate(['Paris','Tokyo'])]
        elif tool_results:
            generated = [{'type':'text','text':'工具结果已接收'}]
        else:
            generated = [{'type':'text','text':'你好 bridge'}]
        events = [{'type':'message_start','message':{'type':'message','id':'msg_smoke','role':'assistant',
                  'model':body['model'],'content':[],'stop_reason':None,'stop_sequence':None,
                  'usage':{'input_tokens':13,'output_tokens':0,'cache_read_input_tokens':4}}}]
        for i,b in enumerate(generated):
            start = dict(b)
            if b['type']=='tool_use':
                start['input']={}
                encoded = json.dumps(b['input'])
                ds = [{'type':'input_json_delta','partial_json':encoded[:8]},
                      {'type':'input_json_delta','partial_json':encoded[8:]}]
            else:
                start['text']=''
                ds = [{'type':'text_delta','text':b['text'][:3]}, {'type':'text_delta','text':b['text'][3:]}]
            events.append({'type':'content_block_start','index':i,'content_block':start})
            events.extend({'type':'content_block_delta','index':i,'delta':d} for d in ds)
            events.append({'type':'content_block_stop','index':i})
        events.extend([{'type':'message_delta','delta':{'stop_reason':'tool_use' if use_tools else 'end_turn','stop_sequence':None},'usage':{'output_tokens':9}}, {'type':'message_stop'}])
        data = ''.join('event: '+e['type']+'\ndata: '+json.dumps(e,ensure_ascii=False)+'\n\n' for e in events).encode()
        self.send_response(200);self.send_header('content-type','text/event-stream');self.send_header('content-length',str(len(data)));self.end_headers()
        try: self.wfile.write(data)
        except (BrokenPipeError,ConnectionResetError): pass

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--cli',required=True)
    parser.add_argument('--bridge',type=Path)
    args=parser.parse_args()
    cli=str(Path(args.cli).expanduser().resolve())
    if args.bridge is None:
        subprocess.run(['cargo','build','--locked'],cwd=ROOT,check=True)
    binary=(args.bridge or ROOT/'target/debug/claude-messages-bridge').resolve()
    version=subprocess.check_output([cli,'--version'],text=True).strip()
    fake=ThreadingHTTPServer(('127.0.0.1',0),FakeApi)
    threading.Thread(target=fake.serve_forever,daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='messages-smoke-') as tmp:
        port=available_port()
        env={'PATH':os.environ['PATH'],'HOME':tmp,'CLAUDE_CONFIG_DIR':tmp+'/config',
             'ANTHROPIC_API_KEY':'dummy-local-test-key','ANTHROPIC_BASE_URL':f'http://127.0.0.1:{fake.server_port}',
             'CLAUDE_CLI_PATH':cli,'BRIDGE_CLI_BARE':'1','BRIDGE_BIND':f'127.0.0.1:{port}',
             'BRIDGE_API_KEY':'smoke-key','BRIDGE_TIMEOUT_SECONDS':'30','RUST_LOG':'warn'}
        logfile=open(Path(tmp)/'bridge.log','w+')
        bridge=subprocess.Popen([str(binary)],env=env,cwd=tmp,stdout=logfile,stderr=logfile)
        def post(payload):
            request=Request(f'http://127.0.0.1:{port}/v1/messages',data=json.dumps(payload).encode(),
                            headers={'content-type':'application/json','x-api-key':'smoke-key','anthropic-version':'2023-06-01'})
            with urlopen(request,timeout=40) as r:
                data=r.read().decode()
                return data if payload.get('stream') else json.loads(data)
        try:
            for _ in range(100):
                try:
                    with urlopen(f'http://127.0.0.1:{port}/healthz',timeout=.2): break
                except OSError: time.sleep(.05)
            base={'model':'claude-sonnet-4-6','max_tokens':128,'system':'Test system prompt',
                  'messages':[{'role':'user','content':'Earlier question'},{'role':'assistant','content':'Earlier answer'},{'role':'user','content':'BASIC'}]}
            result=post(base)
            assert result['content']==[{'type':'text','text':'你好 bridge'}],result
            assert result['usage']['output_tokens']==9,result
            assert result['stop_reason']=='end_turn',result
            with LOCK: upstream=CAPTURED[-1]
            assert upstream['max_tokens']==128,upstream
            assert [m['role'] for m in upstream['messages']]==['user','assistant','user'],upstream
            assert any(b.get('text')=='Earlier answer' for b in content(upstream['messages'][1:2])),upstream
            print('PASS real CLI: JSON response, usage, max_tokens and structured history')
            sse=post({**base,'stream':True})
            assert 'event: message_start' in sse and 'event: message_stop' in sse and '[DONE]' not in sse,sse
            print('PASS real CLI: Anthropic SSE framing')
            tool={'name':'weather','description':'Get weather','input_schema':{'type':'object','properties':{'city':{'type':'string'}},'required':['city']}}
            call={**base,'tools':[tool],'messages':[{'role':'user','content':'USE_TOOL'}]}
            answer=post(call)
            uses=[b for b in answer['content'] if b['type']=='tool_use']
            assert len(uses)==2 and all(b['name']=='weather' for b in uses),answer
            assert answer['stop_reason']=='tool_use',answer
            with LOCK: upstream=CAPTURED[-1]
            assert [t['name'] for t in upstream['tools']]==['mcp__messages__weather'],upstream['tools']
            print('PASS real CLI: SDK MCP RPC registration and parallel tool_use name mapping')
            call['messages'].extend([{'role':'assistant','content':answer['content']},
                {'role':'user','content':[{'type':'tool_result','tool_use_id':b['id'],'content':'sunny'} for b in uses]}])
            result=post(call)
            assert result['content'][0]['text']=='工具结果已接收',(result, CAPTURED[-1]['messages'])
            with LOCK: upstream=CAPTURED[-1]
            past=[b for b in content(upstream['messages']) if b['type']=='tool_use']
            results=[b for b in content(upstream['messages']) if b['type']=='tool_result']
            assert {b['id'] for b in past}=={b['tool_use_id'] for b in results}=={'toolu_0','toolu_1'},upstream
            assert all(b['name']=='mcp__messages__weather' for b in past),past
            assert [m['role'] for m in upstream['messages']]==['user','assistant','user'],upstream
            # CLI appends its own date reminder and cache hint, even with a custom
            # system prompt. This documented CLI behavior must not hide extra turns.
            for b in results:
                c=b['content']
                if isinstance(c,list): c=''.join(part['text'] for part in c)
                assert re.fullmatch(r"sunny(?:\n\n<system-reminder>\nToday's date is \d{4}-\d{2}-\d{2}\.\n</system-reminder>)?",c),b
            print('PASS real CLI: caller tool_result restores into native CLI transcript')
            sse=post({**base,'stream':True,'tools':[tool],'messages':[{'role':'user','content':'USE_TOOL'}]})
            assert 'mcp__messages__' not in sse and 'input_json_delta' in sse and '"tool_use"' in sse,sse
            print('PASS real CLI: tool streaming with original caller names')
            mixed={**call,'stream':False,'messages':list(call['messages'])}
            mixed['messages'][-1]={'role':'user','content':[
                {'type':'tool_result','tool_use_id':'toolu_0','content':[{'type':'text','text':'failed'}],'is_error':True},
                {'type':'tool_result','tool_use_id':'toolu_1','content':'sunny'},
                {'type':'text','text':'Explain both results'}]}
            assert post(mixed)['content'][0]['text']=='工具结果已接收'
            with LOCK: upstream=CAPTURED[-1]
            bs=content(upstream['messages'][-1:])
            assert [b['type'] for b in bs[:3]]==['tool_result','tool_result','text'],bs
            assert bs[0]['is_error'] is True and bs[2]['text']=='Explain both results',bs
            assert len(bs)==3 or (len(bs)==4 and bs[3]['text'].startswith("<system-reminder>\nToday's date is ")),bs
            assert [m['role'] for m in upstream['messages']]==['user','assistant','user'],upstream
            print('PASS real CLI: error tool_result, structured result and accompanying user text')
            none={**base,'tools':[tool],'tool_choice':{'type':'none'}}
            post(none)
            with LOCK: assert not CAPTURED[-1].get('tools'),CAPTURED[-1]
            print('PASS real CLI: tool_choice none removes tool definitions')
            # Detect accidental extra model turns from the CLI's agent loop.
            with LOCK: assert len(CAPTURED)==7, f'Unexpected model calls: {len(CAPTURED)}'
            print(f'All smoke checks passed ({version}); 7 local fake API calls, 0 real model calls.')
        except Exception:
            logfile.flush();logfile.seek(0);print(logfile.read())
            raise
        finally:
            bridge.terminate()
            try: bridge.wait(timeout=5)
            except subprocess.TimeoutExpired: bridge.kill();bridge.wait()
            logfile.close();fake.shutdown();fake.server_close()

if __name__=='__main__':main()
