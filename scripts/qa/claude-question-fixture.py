#!/usr/bin/env python3
"""Actual Claude Code TUI with a synthetic localhost Anthropic server and own HOME.
Safe mode and setting-sources isolate personal customizations; an explicitly
accepted fake API key and localhost base URL isolate model calls. Bare mode
omits AskUserQuestion in Claude 2.1.258, so it cannot exercise this picker.
"""
import argparse,http.server,json,os,pathlib,shlex,shutil,subprocess,tempfile,threading,time
p=argparse.ArgumentParser();p.add_argument('--probe');p.add_argument('--inspect',action='store_true');p.add_argument('--keep',action='store_true');p.add_argument('--output',default='/tmp/jarvis-claude-question-fixture.json');args=p.parse_args()
root=pathlib.Path(tempfile.mkdtemp(prefix='jq-',dir='/tmp'));home=root/'home';work=root/'work';config=root/'claude'
for d in (home,work,config):d.mkdir()
for path in (home/'.claude.json', config/'.claude.json'):
 path.write_text(json.dumps({'hasCompletedOnboarding':True,'lastOnboardingVersion':'2.1.258','theme':'dark','projects':{str(work.resolve()):{'hasTrustDialogAccepted':True,'hasCompletedProjectOnboarding':True}},'customApiKeyResponses':{'approved':['qa-fixture-not-a-real-key','qa-fixture-not-a-real-key'[-20:]],'rejected':[]}}))
questions={'questions':[
 {'header':'Machine','question':'Where should the synthetic checks run?','multiSelect':False,'options':[{'label':'Local','description':'This isolated machine'},{'label':'VM','description':'A synthetic VM'}]},
 {'header':'Checks','question':'Which synthetic checks are required?','multiSelect':True,'options':[{'label':'Unit','description':'Fast isolated tests'},{'label':'UI','description':'Visual checks'},{'label':'Integration','description':'Transport checks'}]},
 {'header':'Constraint','question':'Which extra constraint should we use?','multiSelect':False,'options':[{'label':'Fast','description':'Run quickly'},{'label':'Thorough','description':'Inspect carefully'}]}
]}
requests=[];accepted=[]
class Handler(http.server.BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1'
 def log_message(self,*a):pass
 def do_GET(self):self.respond_json({'ok':True})
 def respond_json(self,obj):
  b=json.dumps(obj).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(b)));self.end_headers();self.wfile.write(b)
 def do_POST(self):
  data=json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))));tools=[t.get('name') for t in data.get('tools',[])];outputs=[]
  for message in data.get('messages',[]):
   if isinstance(message.get('content'),list):outputs.extend(c for c in message['content'] if c.get('type')=='tool_result')
  requests.append({'path':self.path,'tools':tools,'toolResults':outputs})
  if 'count_tokens' in self.path:self.respond_json({'input_tokens':24});return
  if '/messages' not in self.path:self.respond_json({});return
  answered=any(o.get('tool_use_id')=='toolu_question_qa' for o in outputs)
  if answered:accepted.extend(o for o in outputs if o.get('tool_use_id')=='toolu_question_qa')
  if answered or 'AskUserQuestion' not in tools:
   block={'type':'text','text':''};delta={'type':'text_delta','text':'Synthetic answers accepted.'};stop='end_turn'
  else:
   block={'type':'tool_use','id':'toolu_question_qa','name':'AskUserQuestion','input':{}};delta={'type':'input_json_delta','partial_json':json.dumps(questions)};stop='tool_use'
  events=[('message_start',{'message':{'id':'msg_qa','type':'message','role':'assistant','content':[],'model':'claude-sonnet-4-6','stop_reason':None,'stop_sequence':None,'usage':{'input_tokens':24,'output_tokens':0}}}),('content_block_start',{'index':0,'content_block':block}),('content_block_delta',{'index':0,'delta':delta}),('content_block_stop',{'index':0}),('message_delta',{'delta':{'stop_reason':stop,'stop_sequence':None},'usage':{'output_tokens':12}}),('message_stop',{})]
  b=''.join('event: '+kind+'\ndata: '+json.dumps({'type':kind,**obj})+'\n\n' for kind,obj in events).encode();self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(b)));self.end_headers();self.wfile.write(b)
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);server.daemon_threads=True;threading.Thread(target=server.serve_forever,daemon=True).start()
binary='/Users/se.chernyshev/.nvm/versions/node/v22.17.0/bin/claude';socket=root/'tmux.sock';tmux=shutil.which('tmux')
def t(*a):return subprocess.run([tmux,'-u','-S',str(socket),*a],capture_output=True,text=True,check=True).stdout
def screen():return t('capture-pane','-p','-t','%0')
def wait_for(predicate,seconds=20):
 end=time.time()+seconds
 while time.time()<end:
  value=screen()
  if predicate(value):return value
  time.sleep(.15)
 raise RuntimeError('Expected TUI not observed:\n'+screen())
env={'HOME':str(home),'CLAUDE_CONFIG_DIR':str(config),'PATH':'/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin','TERM':'xterm-256color','LANG':'en_US.UTF-8','JARVIS_IGNORE':'1','TMPDIR':str(root),'ANTHROPIC_API_KEY':'qa-fixture-not-a-real-key','ANTHROPIC_BASE_URL':f'http://127.0.0.1:{server.server_address[1]}','DISABLE_TELEMETRY':'1','DISABLE_ERROR_REPORTING':'1','DISABLE_AUTOUPDATER':'1','DISABLE_NON_ESSENTIAL_MODEL_CALLS':'1'}
command=[binary,'--safe-mode','--strict-mcp-config','--setting-sources','','--permission-mode','plan','--model','sonnet','--tools','AskUserQuestion','--','Ask the synthetic questions now.']
launch='cd '+shlex.quote(str(work))+' && env -i '+' '.join(shlex.quote(k+'='+v) for k,v in env.items())+' '+' '.join(map(shlex.quote,command))+'; qa_fixture_exit=$?; echo "QA fixture process exited: $qa_fixture_exit"; sleep 60'
result={'root':str(root),'binary':binary,'requests':requests,'accepted':accepted}
try:
 t('new-session','-d','-s','question-qa','-x','140','-y','45',launch)
 value=wait_for(lambda s:'Where should the synthetic checks run?' in s or 'trust' in s.lower() or 'Choose the text style' in s or 'security' in s.lower(),25)
 if 'Where should the synthetic checks run?' not in value:
  (root/'startup-screen.txt').write_text(value)
  if 'trust' in value.lower() or 'security' in value.lower():
   # This is exclusively the empty temporary workspace created above.
   time.sleep(1)
   if '❯ No, exit' in value:t('send-keys','-t','%0','Down');time.sleep(.5)
   t('send-keys','-t','%0','Enter')
  value=wait_for(lambda s:'Where should the synthetic checks run?' in s,25)
 result['initialScreen']=value;(root/'question-screen.txt').write_text(value)
 pathlib.Path(args.output).write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n');print(json.dumps({'ready':True,'root':str(root),'report':args.output}),flush=True)
 if args.inspect:
  time.sleep(.6);t('send-keys','-t','%0','Enter')
  value=wait_for(lambda s:'Which synthetic checks are required?' in s);time.sleep(.3)
  t('send-keys','-t','%0','Space');time.sleep(.2);(root/'multi-screen.txt').write_text(screen())
  t('send-keys','-t','%0','Tab');wait_for(lambda s:'Which extra constraint should we use?' in s)
  time.sleep(.3);t('send-keys','-t','%0','Down');t('send-keys','-t','%0','Down');time.sleep(.3)
  t('set-buffer','-b','qa-text','--','Первая строка\nВторая строка');t('paste-buffer','-p','-d','-b','qa-text','-t','%0');time.sleep(.4)
  (root/'custom-screen.txt').write_text(screen());t('send-keys','-t','%0','Enter');time.sleep(.4);(root/'review-screen.txt').write_text(screen())
  print(json.dumps({'inspected':True,'root':str(root)}),flush=True)
 if args.probe:
  def plan_and_send(picks,index,text=None):
   (root/'plan-request.json').write_text(json.dumps({'agent':'claude','questionCount':3,'questionIndex':index,'screen':screen(),'picks':picks,'text':text},ensure_ascii=False))
   run=subprocess.run([args.probe,'tmux::answer_keys_tests::isolated_codex_tui_plan_probe','--exact','--ignored','--nocapture'],env={**os.environ,'JARVIS_QA_QUESTION_FIXTURE':str(root)},capture_output=True,text=True)
   if run.returncode:raise RuntimeError('Production planner rejected actual Claude TUI: '+run.stdout+run.stderr)
   plan=json.loads((root/'plan.json').read_text());result.setdefault('plans',[]).append(plan)
   for step in plan['keys']:
    if 'key' in step:t('send-keys','-t','%0',step['key'])
    else:
     name='qa-answer-'+str(time.time_ns());t('set-buffer','-b',name,'--',step['text']);t('paste-buffer','-p','-d','-b',name,'-t','%0');time.sleep(.09)
    time.sleep(.14)
  t('send-keys','-t','%0','Down');time.sleep(.2);plan_and_send([1],0)
  wait_for(lambda s:'Which synthetic checks are required?' in s)
  # Preselect Unit in the actual terminal before Jarvis requests Unit+Integration.
  t('send-keys','-t','%0','Space');t('send-keys','-t','%0','Down');time.sleep(.2);plan_and_send([1,3],1)
  wait_for(lambda s:'Which extra constraint should we use?' in s)
  custom='Первая строка: без сети.\nВторая строка: сохранить черновик.';plan_and_send([],2,custom)
  review=wait_for(lambda s:'Submit answers' in s or 'Submit' in s)
  result['reviewScreen']=review;t('send-keys','-t','%0','1')
  end=time.time()+15
  while time.time()<end and not accepted:time.sleep(.15)
  if not accepted:raise RuntimeError('Claude did not submit answers: '+screen())
  result['confirmedAnswers']=accepted[-1];result['finalScreen']=screen();result['ok']=True
  encoded=json.dumps(accepted[-1],ensure_ascii=False)
  for required in ['Local','Unit','Integration','Первая строка','Вторая строка']:assert required in encoded,accepted[-1]
  pathlib.Path(args.output).write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n');print(json.dumps({'ok':True,'confirmedAnswers':accepted[-1],'report':args.output},ensure_ascii=False),flush=True)
 if args.keep:
  end=time.time()+600
  while time.time()<end and not (root/'finish').exists():time.sleep(.2)
except Exception as error:
 result['ok']=False;result['error']=str(error)
 try:result['screen']=screen()
 except Exception:pass
 pathlib.Path(args.output).write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n');print(json.dumps({'ok':False,'error':str(error),'root':str(root)},ensure_ascii=False),flush=True);raise
finally:
 try:t('kill-server')
 except Exception:pass
 server.shutdown();server.server_close()
