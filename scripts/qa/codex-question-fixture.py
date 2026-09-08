#!/usr/bin/env python3
"""Real isolated Codex TUI against a localhost-only synthetic Responses API.
No auth files, user CODEX_HOME, ordinary tmux server, or external service is used.
--keep leaves the owned detached pane available for a production planner probe.
"""
import argparse, http.server, json, os, pathlib, shlex, shutil, subprocess, tempfile, threading, time

parser=argparse.ArgumentParser(); parser.add_argument('--keep',action='store_true'); parser.add_argument('--probe',help='Compiled jarvis test executable with isolated_codex_tui_plan_probe'); parser.add_argument('--output',default='/tmp/jarvis-codex-question-fixture.json'); args=parser.parse_args()
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
        data=json.loads(raw);requests.append({'path':self.path,'tools':[t.get('name') for t in data.get('tools',[])],'functionOutputs':[i for i in data.get('input',[]) if i.get('type')=='function_call_output']})
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
config=f'''model = "gpt-5.5"
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
    t('new-session','-d','-s','question-qa','-x','140','-y','45',launch)
    value=wait_for(lambda s:'Ask Codex to do anything' in s or 'context left' in s or '/help' in s or 'Try ' in s or 'Select a model' in s,25)
    t('send-keys','-t','%0','-l','/plan');time.sleep(.4);t('send-keys','-t','%0','Enter');wait_for(lambda s:'plan' in s.lower() and '/plan' not in s,10)
    t('send-keys','-t','%0','-l','Ask the synthetic question now.');time.sleep(.4);t('send-keys','-t','%0','Enter')
    value=wait_for(lambda s:'Where should the synthetic checks run?' in s or 'Synthetic answers accepted' in s,30)
    (root/'question-screen.txt').write_text(value)
    result['screen']=value; result['ok']='Where should the synthetic checks run?' in value
    pathlib.Path(args.output).write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps({'ok':result['ok'],'root':str(root),'report':args.output,'requests':len(requests),'accepted':accepted},ensure_ascii=False),flush=True)
    if args.probe:
        def plan_and_send(picks,text=None):
            capture=screen()
            (root/'plan-request.json').write_text(json.dumps({'screen':capture,'picks':picks,'text':text},ensure_ascii=False))
            probe_env={**os.environ,'JARVIS_QA_QUESTION_FIXTURE':str(root)}
            run=subprocess.run([args.probe,'tmux::answer_keys_tests::isolated_codex_tui_plan_probe','--exact','--ignored','--nocapture'],env=probe_env,capture_output=True,text=True)
            if run.returncode: raise RuntimeError('Production planner rejected actual TUI: '+run.stdout+run.stderr)
            planned=json.loads((root/'plan.json').read_text());result.setdefault('plans',[]).append(planned)
            for step in planned['keys']:
                if 'key' in step: t('send-keys','-t','%0',step['key'])
                else:
                    name='qa-answer-'+str(time.time_ns());t('set-buffer','-b',name,'--',step['text']);t('paste-buffer','-p','-d','-b',name,'-t','%0');time.sleep(.09)
                time.sleep(.14)
        # Deliberately move away from the first option before using the planner.
        t('send-keys','-t','%0','Down');time.sleep(.2)
        plan_and_send([1])
        wait_for(lambda s:'Question 2/2' in s,10)
        t('send-keys','-t','%0','Down');time.sleep(.2)
        custom='Первая строка: без сети.\nВторая строка: сохранять черновик.'
        plan_and_send([],custom)
        end=time.time()+15
        while time.time()<end and not accepted: time.sleep(.15)
        if not accepted: raise RuntimeError('Codex did not return accepted tool answers: '+screen())
        response=json.loads(accepted[-1]['output'])
        assert response['answers']['machine']['answers']==['Local'],response
        assert any(custom in value for value in response['answers']['notes']['answers']),response
        result['confirmedAnswers']=response;result['ok']=True;result['finalScreen']=screen()
        pathlib.Path(args.output).write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
        print(json.dumps({'ok':True,'confirmedAnswers':response,'report':args.output},ensure_ascii=False),flush=True)
    if args.keep:
        print('Owned fixture ready; write '+str(root/'finish')+' to stop.',flush=True)
        until=time.time()+600
        while time.time()<until and not (root/'finish').exists():
            pathlib.Path(args.output).write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
            time.sleep(.3)
except Exception as error:
    result['ok']=False;result['error']=str(error)
    try: result['screen']=screen()
    except Exception: pass
    pathlib.Path(args.output).write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps({'ok':False,'error':str(error),'root':str(root),'report':args.output},ensure_ascii=False),flush=True)
    raise
finally:
    try: t('kill-server')
    except Exception: pass
    server.shutdown();server.server_close()
