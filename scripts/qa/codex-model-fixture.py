#!/usr/bin/env python3
"""Real isolated Codex TUI against a localhost-only synthetic Responses API.
No auth files, user CODEX_HOME, ordinary tmux server, or external service is used.
--keep leaves the owned detached pane available for a production planner probe.
"""
import argparse, http.server, json, os, pathlib, shlex, shutil, subprocess, tempfile, threading, time

parser=argparse.ArgumentParser(); parser.add_argument('--keep',action='store_true'); parser.add_argument('--probe',help='Compiled jarvis test executable with ipc::model_picker::tests::isolated_codex_model_probe'); parser.add_argument('--output',default='/tmp/jarvis-codex-model-fixture.json'); args=parser.parse_args()
root=pathlib.Path(tempfile.mkdtemp(prefix='jq-',dir='/tmp')); home=root/'home'; codex_home=root/'codex'; work=root/'work'
for d in (home,codex_home,work): d.mkdir()
requests=[]; accepted=[]
question={'questions':[{'id':'machine','header':'Machine','question':'Where should the synthetic checks run?','options':[{'label':'Local','description':'This isolated computer'},{'label':'VM','description':'An isolated remote machine'}]},{'id':'notes','header':'Notes','question':'Which extra synthetic constraint should we use?','options':[{'label':'Fast','description':'Quick check'},{'label':'Thorough','description':'Detailed check'}]}]}
class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def log_message(self,*unused): pass
    def do_GET(self):
        body=json.dumps({'data':[]}).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
    def do_POST(self):
        raw=self.rfile.read(int(self.headers.get('Content-Length','0')))
        if self.headers.get('Content-Encoding')=='zstd':
            try:
                import zstandard
                raw=zstandard.ZstdDecompressor().decompress(raw)
            except ImportError:
                raw=subprocess.run(['zstd','-d','-c'],input=raw,stdout=subprocess.PIPE,check=True).stdout
        data=json.loads(raw);requests.append({'path':self.path,'model':data.get('model'),'tools':[t.get('name') for t in data.get('tools',[])],'functionOutputs':[i for i in data.get('input',[]) if i.get('type')=='function_call_output']})
        outputs=[i for i in data.get('input',[]) if i.get('type')=='function_call_output']
        if outputs or not any(t.get('name')=='request_user_input' for t in data.get('tools',[])):
            accepted.extend(o for o in outputs if o.get('call_id')=='call_question_qa' and o.get('output','').startswith('{'))
            item={'id':'msg_qa','type':'message','role':'assistant','status':'completed','content':[{'type':'output_text','text':'Synthetic answers accepted.','annotations':[]}]}
        else:
            item={'id':'fc_qa','type':'function_call','call_id':'call_question_qa','name':'request_user_input','arguments':json.dumps(question),'status':'completed'}
        events=[('response.created',{'response':{'id':'resp_qa','status':'in_progress','output':[]}}),('response.output_item.added',{'output_index':0,'item':item}),('response.output_item.done',{'output_index':0,'item':item}),('response.completed',{'response':{'id':'resp_qa','status':'completed','output':[item],'usage':{'input_tokens':12,'output_tokens':8,'total_tokens':20}}})]
        body=''.join('event: '+kind+'\ndata: '+json.dumps({'type':kind,**value})+'\n\n' for kind,value in events).encode()
        self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);server.daemon_threads=True;threading.Thread(target=server.serve_forever,daemon=True).start()
config=f'''model = "gpt-5.6-sol"
model_provider = "fixture"
approval_policy = "never"
sandbox_mode = "read-only"
check_for_update_on_startup = false
[model_providers.fixture]
name = "Local synthetic QA"
base_url = "http://127.0.0.1:{server.server_address[1]}/v1"
wire_api = "responses"
requires_openai_auth = false
request_max_retries = 0
stream_max_retries = 0
[projects.{json.dumps(str(work))}]
trust_level = "trusted"
'''
(codex_home/'config.toml').write_text(config)
binary='/Applications/ChatGPT.app/Contents/Resources/codex'; socket=root/'tmux.sock'; tmux=shutil.which('tmux')
def t(*argv): return subprocess.run([tmux,'-u','-S',str(socket),*argv],text=True,capture_output=True,check=True).stdout

def screen(): return t('capture-pane','-p','-t','%0')
def wait_for(test,seconds=15):
    end=time.time()+seconds
    while time.time()<end:
        value=screen()
        if test(value): return value
        time.sleep(.15)
    raise RuntimeError('Expected TUI state not observed:\n'+screen())

env={'HOME':str(home),'CODEX_HOME':str(codex_home),'PATH':'/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin','TERM':'xterm-256color','LANG':'en_US.UTF-8','JARVIS_IGNORE':'1','TMPDIR':str(root)}
launch='exec env -i '+' '.join(shlex.quote(k+'='+v) for k,v in env.items())+' '+shlex.quote(binary)+' --no-alt-screen -C '+shlex.quote(str(work))
result={'root':str(root),'socket':str(socket),'codexHome':str(codex_home),'binary':binary,'requests':requests,'accepted':accepted}
try:
    if not args.probe: raise RuntimeError('--probe must point to the compiled Jarvis test executable')
    (root/'owned-model-fixture').touch();(root/'requested-model').write_text('gpt-5.6-sol')
    bin_dir=root/'bin';bin_dir.mkdir()
    wrapper=bin_dir/'tmux'
    wrapper.write_text('#!/usr/bin/python3\nimport os,sys\nargs=sys.argv[1:]\nif "-L" in args:\n i=args.index("-L"); assert args[i+1]=="jarvis"; args[i:i+2]=["-S",'+repr(str(socket))+']\nos.execv('+repr(tmux)+',['+repr(tmux)+',*args])\n')
    wrapper.chmod(0o755)
    launch += ' --model gpt-5.6-terra'
    t('new-session','-d','-s','model-qa','-x','140','-y','45',launch)
    wait_for(lambda s:'Ask Codex to do anything' in s and 'loading' not in s,25)
    result['initialScreen']=screen()
    probe_env={**os.environ,'PATH':str(bin_dir)+':'+os.environ['PATH'],'JARVIS_QA_MODEL_FIXTURE':str(root),'HOME':str(home),'CODEX_HOME':str(codex_home)}
    assert 'gpt-5.6-terra' in result['initialScreen'],result['initialScreen']
    draft='Preserve this native draft exactly.'
    t('send-keys','-t','%0','-l',draft);time.sleep(.3)
    (root/'expect-refusal').touch()
    refused=subprocess.run([args.probe,'ipc::model_picker::tests::isolated_codex_model_probe','--exact','--ignored','--nocapture'],env=probe_env,capture_output=True,text=True)
    assert refused.returncode==0,refused.stdout+refused.stderr
    assert draft in screen() and not requests,'Draft was changed or sent'
    result['draftRefused']=True
    (root/'expect-refusal').unlink();t('send-keys','-t','%0','C-u');time.sleep(.3)
    probe=subprocess.run([args.probe,'ipc::model_picker::tests::isolated_codex_model_probe','--exact','--ignored','--nocapture'],env=probe_env,capture_output=True,text=True)
    result['probeOutput']=probe.stdout+probe.stderr
    result['selectedScreen']=screen()
    if probe.returncode: raise RuntimeError(result['probeOutput']+'\n'+screen())
    t('send-keys','-t','%0','-l','Synthetic local model verification');time.sleep(.3);t('send-keys','-t','%0','Enter')
    wait_for(lambda s:'Synthetic answers accepted' in s,20)
    assert requests and all(request['model']=='gpt-5.6-sol' for request in requests),requests
    result['finalScreen']=screen();result['ok']=True
    pathlib.Path(args.output).write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps({'ok':True,'root':str(root),'requests':requests,'report':args.output},ensure_ascii=False))
except Exception as error:
    result['ok']=False;result['error']=str(error)
    pathlib.Path(args.output).write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    raise
finally:
    try:t('kill-server')
    except Exception:pass
    server.shutdown();server.server_close()
