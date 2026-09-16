#!/usr/bin/env python3
"""Deterministic stdio CLI fixture. No network or real tools."""
import json, os, sys, time
from pathlib import Path
mode = os.environ.get('FAKE_MODE', 'text')
if p := os.environ.get('FAKE_PID'): Path(p).write_text(str(os.getpid()))
def emit(frame):
    sys.stdout.write(json.dumps(frame, ensure_ascii=False) + '\n'); sys.stdout.flush()
def receive():
    line = sys.stdin.readline()
    if not line: sys.exit(0)
    frame = json.loads(line)
    if p := os.environ.get('FAKE_TRACE'):
        with open(p, 'a') as f: f.write(json.dumps(frame) + '\n')
    return frame
def event(value): emit({'type':'stream_event','parent_tool_use_id':None,'event':value})
init = receive()
assert init['request']['subtype'] == 'initialize'
if mode == 'managed':
    assert os.environ['CLAUDE_CODE_OAUTH_TOKEN']=='saved-access'
    assert 'ANTHROPIC_API_KEY' not in os.environ
    assert 'CLAUDE_CODE_OAUTH_REFRESH_TOKEN' not in os.environ
    assert 'BRIDGE_ADMIN_TOKEN' not in os.environ
    assert not (Path(os.environ['CLAUDE_CONFIG_DIR']) / '.credentials.json').exists()
if mode == 'init_hang': time.sleep(60); sys.exit(0)
if mode == 'init_error':
    emit({'type':'control_response','response':{'subtype':'error','request_id':init['request_id'],'error':'fixture error'}}); sys.exit(0)
if init['request']['sdkMcpServers']:
    emit({'type':'control_request','request_id':'mcp-list','request':{'subtype':'mcp_message','server_name':'messages','message':{'jsonrpc':'2.0','id':7,'method':'tools/list'}}})
    reply=receive()
    assert reply['response']['request_id']=='mcp-list'
    assert reply['response']['response']['mcp_response']['result']['tools'][0]['name']=='weather'
emit({'type':'control_response','response':{'subtype':'success','request_id':init['request_id'],'response':{}}})
user=receive()
assert user['type']=='user'
if mode == 'history':
    path = sys.argv[sys.argv.index('--resume')+1]
    rows=[json.loads(l) for l in Path(path).read_text().splitlines()]
    assert rows[0]['message']['content'][0]['text']=='Earlier'
    assert rows[1]['message']['content'][0]['name']=='mcp__messages__weather'
    assert rows[2]['message']['content'][0]['tool_use_id']=='toolu_prior'
    assert user['message']['content'][0]['tool_use_id']=='toolu_prior'
    assert sys.argv[sys.argv.index('--resume-session-at')+1]==rows[1]['uuid']
if mode == 'crash': sys.exit(7)
if mode == 'max_frame': print('x'*10000,flush=True); sys.exit(0)
if mode == 'error_before':
    emit({'type':'result','is_error':True,'subtype':'error_during_execution','errors':['fixture upstream failure']});sys.exit(0)
# Exercise draining stderr and filtering non-JSON stdout/internal events.
sys.stderr.write('diagnostic '*12000);sys.stderr.flush()
print('non-json diagnostic', flush=True)
emit({'type':'keep_alive'})
emit({'type':'stream_event','parent_tool_use_id':'child-tool','event':{'type':'message_start','message':{'id':'wrong'}}})
event({'type':'message_start','message':{'id':'msg_fixture','type':'message','role':'assistant','model':'test-model','content':[],'stop_reason':None,'stop_sequence':None,'usage':{'input_tokens':11,'output_tokens':0,'cache_read_input_tokens':5}}})
if mode == 'hang': time.sleep(60);sys.exit(0)
if mode == 'error_mid':
    emit({'type':'result','is_error':True,'errors':['fixture stream failure']});sys.exit(0)
tool=mode in ('tool','bad_tool_json')
if tool:
    block={'type':'tool_use','id':'toolu_fixture','name':'mcp__messages__weather','input':{}}
    deltas=[{'type':'input_json_delta','partial_json':'{"city":'}, {'type':'input_json_delta','partial_json':'"Paris"}' if mode!='bad_tool_json' else 'broken'}]
else:
    block={'type':'text','text':''};deltas=[{'type':'text_delta','text':'你好 '},{'type':'text_delta','text':'bridge'}]
event({'type':'content_block_start','index':0,'content_block':block})
for delta in deltas:event({'type':'content_block_delta','index':0,'delta':delta})
event({'type':'content_block_stop','index':0})
# Snapshot must not be rendered twice.
emit({'type':'assistant','message':{'content':[{'type':'text','text':'DUPLICATE'}]}})
event({'type':'message_delta','delta':{'stop_reason':'tool_use' if tool else 'end_turn','stop_sequence':None},'usage':{'output_tokens':7}})
event({'type':'message_stop'})
# Remain alive until bridge stops us; useful for process cancellation/reaping checks.
time.sleep(60)
