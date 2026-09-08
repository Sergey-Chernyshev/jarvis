#!/usr/bin/env python3
"""Real isolated node/tmux/hook pipeline with synthetic provider homes only.

No provider CLI, user auth, ~/.ssh config, existing tmux or VM is changed.
Can run unchanged inside Linux after copying this script, bin/jarvis-hook and
an appropriate jarvis-node executable. Output contains only synthetic data.
"""
import argparse,json,os,pathlib,pty,shlex,shutil,socket,subprocess,sys,tempfile,time,urllib.error,urllib.parse,urllib.request
p=argparse.ArgumentParser();p.add_argument('--node',required=True);p.add_argument('--hook',required=True);p.add_argument('--output');p.add_argument('--codex-bin');args=p.parse_args()
root=pathlib.Path(tempfile.mkdtemp(prefix='jarvis-node-parity-',dir='/tmp')).resolve();root.chmod(0o700)
node_dir=root/'node';node_dir.mkdir();home=root/'home';home.mkdir();runtime=root/'tmux-runtime';runtime.mkdir();repo=root/'repo';repo.mkdir()
profiles=[home/'codex-personal',home/'codex-work'];sources=[]
for profile in profiles:
 (profile/'sessions').mkdir(parents=True)
 (profile/'sessions/same.jsonl').write_text(json.dumps({'type':'session_meta','payload':{'id':'same-id','cwd':str(repo)}})+'\n'+json.dumps({'type':'event_msg','payload':{'type':'token_count','info':{'total_token_usage':{'input_tokens':100,'output_tokens':20},'last_token_usage':{'input_tokens':100,'output_tokens':20}}}})+'\n')
 (profile/'auth.json').write_text('SYNTHETIC_PRIVATE_VALUE')
 sources.append({'agent':'codex','providerHome':str(profile)})
(node_dir/'provider-roots.json').write_text(json.dumps({'version':1,'sources':sources}))
(node_dir/'bin').mkdir();hook=node_dir/'bin/jarvis-hook';hook.write_text(pathlib.Path(args.hook).read_text().replace('/run.sock}','/node.sock}'));hook.chmod(0o755)
shims=node_dir/'shims';shims.mkdir();shim=shims/'codex';shim.write_text(pathlib.Path(args.hook).with_name('agent-shim').read_text().replace('JARVIS_SOCK=${JARVIS_SOCK:-$JARVIS_DIR/run.sock}','JARVIS_SOCK=${JARVIS_SOCK:-$JARVIS_DIR/node.sock}'));shim.chmod(0o755)
(node_dir/'tmux.conf').write_text(pathlib.Path(args.hook).with_name('jarvis-tmux.conf').read_text())
real_bin=home/'.local/bin';real_bin.mkdir(parents=True);fake=real_bin/'codex';shim_state=repo/'shim-environment.json'
fake.write_text('#!'+sys.executable+'\nimport os,json,pathlib,subprocess,sys,time\n'
 'if len(sys.argv)>1 and sys.argv[1]=="app-server":\n'
 ' binary='+repr(str(pathlib.Path(args.codex_bin).resolve()) if args.codex_bin else '')+'\n'
 ' if not binary: sys.exit(3)\n'
 ' os.execv(binary,[binary]+sys.argv[1:])\n'
 'pathlib.Path('+repr(str(shim_state))+').write_text(json.dumps({k:os.environ.get(k) for k in ["CODEX_HOME","JARVIS_SOCK","JARVIS_PROVIDER_INSTANCE_ID"]}))\n'
 'subprocess.run(['+repr(str(hook))+',"codex","session-start"],input=json.dumps({"session_id":"shim-session","cwd":os.getcwd()}),text=True)\n'
 'print("SHIM_READY",flush=True)\n'
 'time.sleep(300)\n');fake.chmod(0o755)
receiver=repo/'receiver.py';received=repo/'received.jsonl';state=repo/'environment.json'
receiver.write_text('import os,json,sys,pathlib\npathlib.Path("environment.json").write_text(json.dumps({"home":os.environ.get("CODEX_HOME"),"instance":os.environ.get("JARVIS_PROVIDER_INSTANCE_ID")}))\nprint("PARITY_READY",flush=True)\nfor line in sys.stdin:\n with open("received.jsonl","a") as f: f.write(json.dumps(line)+"\\n")\n print("PARITY_RECEIVED",flush=True)\n')
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
env=dict(os.environ,HOME=str(home),JARVIS_DIR=str(node_dir),JARVIS_NODE_TCP=f'127.0.0.1:{port}',TMUX_TMPDIR=str(runtime),CODEX_HOME=str(profiles[0]),CLAUDE_CONFIG_DIR=str(home/'.claude'),LC_ALL='C')
env.pop('TMUX',None);env.pop('JARVIS_IGNORE',None);env.pop('JARVIS_SOCK',None)
base=f'http://127.0.0.1:{port}';results=[]
def request(path,body=None):
 data=None if body is None else json.dumps(body,ensure_ascii=False).encode()
 req=urllib.request.Request(base+path,data=data,headers={'Content-Type':'application/json'})
 try:
  with urllib.request.urlopen(req,timeout=8) as res:
   raw=res.read();return res.status,json.loads(raw) if raw else None
 except urllib.error.HTTPError as e:return e.code,json.loads(e.read())
def check(name,condition,details=None):
 row={'scenario':name,'passed':bool(condition),'details':details};results.append(row);print(json.dumps(row,ensure_ascii=False),flush=True)
 if not condition:raise AssertionError(name)
def wait(fn,timeout=8):
 end=time.monotonic()+timeout
 while time.monotonic()<end:
  try:
   value=fn()
   if value:return value
  except (OSError,ValueError):pass
  time.sleep(.05)
 raise RuntimeError('Fixture readiness timeout')
child=None;shim_child=None;pty_master=None;completed=False
try:
 with open(root/'node.log','wb') as log:child=subprocess.Popen([str(pathlib.Path(args.node).resolve())],env=env,stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT)
 hello=wait(lambda:request('/hello')[1]);check('running protocol advertises per-source capabilities',hello['protocol']==2 and 'sessions' in hello['capabilities'])
 _,catalog=request('/sources');work=next(source for source in catalog['sources'] if source['providerHome']==str(profiles[1]));personal=next(source for source in catalog['sources'] if source['providerHome']==str(profiles[0]))
 check('two Codex homes have distinct stable identities',work['id']!=personal['id'])
 _,catalog=request('/sessions');rows=[row for row in catalog['sessions'] if row['id']=='same-id'];check('same raw session ID remains distinct across homes',len(rows)==2 and len({row['sourceId'] for row in rows})==2 and not catalog['limited'])
 _,projects=request('/projects');check('project history preserves source identities',len([row for row in projects['projects'] if row['cwd']==str(repo)])==2)
 code,body=request('/file?'+urllib.parse.urlencode({'path':str(profiles[1]/'auth.json')}));check('provider auth file is outside transcript read boundary',code==403)
 code,body=request('/file?'+urllib.parse.urlencode({'path':str(profiles[1]/'sessions/same.jsonl')}));check('configured nondefault transcript is readable',code==200 and 'same-id' in body['data'])
 before=request('/hello')[1]['cursor'];payload={'session_id':'same-id','transcript_path':str(profiles[1]/'sessions/same.jsonl'),'cwd':str(repo)}
 hook_env=dict(env,CODEX_HOME=str(profiles[1]));out=subprocess.run([str(hook),'codex','session-start'],env=hook_env,input=json.dumps(payload),text=True,capture_output=True,timeout=5)
 code,page=request('/events?since='+str(before));event=page['events'][0]
 check('actual installed hook reaches node with canonical instance identity',out.returncode==0 and event['envelope']['providerHome']==str(profiles[1]) and event['envelope']['instanceId']==work['id'] and event['at']>0 and event['cursor']==before)
 code,launch=request('/launch',{'cwd':str(repo),'cmd':'python3 '+shlex.quote(str(receiver)),'name':'parity','sourceId':work['id']});check('source-targeted launch receives a real pane',code==200 and launch.get('ok') and launch.get('pane'))
 pane=launch['pane'];wait(lambda:state.exists());runtime_env=json.loads(state.read_text());check('launch selects requested account instead of inherited account',runtime_env=={'home':str(profiles[1]),'instance':work['id']})
 check('live remote terminal screen is available',wait(lambda:'PARITY_READY' in request('/screen?'+urllib.parse.urlencode({'pane':pane}))[1].get('screen','')))
 text='Ответ с пробелами, "кавычками" и $(literal)';code,ack=request('/reply',{'pane':pane,'text':text});wait(lambda:received.exists() and received.stat().st_size>0);lines=[json.loads(line) for line in received.read_text().splitlines()]
 check('written answer has acknowledgement and arrives literally',code==200 and ack.get('ok') and text+'\n' in lines)
 key_text='Выбор через keys';code,ack=request('/keys',{'pane':pane,'keys':[{'text':key_text},{'key':'Enter'}]});wait(lambda:key_text+'\n' in [json.loads(line) for line in received.read_text().splitlines()]);check('remote key plan is acknowledged only after literal delivery',code==200 and ack.get('ok'))
 # Keep the node tmux server's stale values deliberately: the managed shim
 # must override them for the new account/session, including node.sock.
 for name,value in [('CODEX_HOME',str(profiles[0])),('JARVIS_SOCK',str(root/'stale.sock'))]:
  subprocess.run([shutil.which('tmux'),'-L','jarvis','set-environment','-g',name,value],env=env,check=True)
 pty_master,slave=pty.openpty();before=request('/hello')[1]['cursor'];shim_env=dict(env,PATH=str(shims)+':'+str(real_bin)+':'+env.get('PATH',''),CODEX_HOME=str(profiles[1]),JARVIS_PROVIDER_INSTANCE_ID=work['id'],TERM='xterm-256color')
 shim_child=subprocess.Popen([str(shim)],env=shim_env,cwd=repo,stdin=slave,stdout=slave,stderr=slave);os.close(slave)
 wait(lambda:shim_state.exists());managed_env=json.loads(shim_state.read_text());check('managed remote shim replaces stale socket and account on existing server',managed_env=={'CODEX_HOME':str(profiles[1]),'JARVIS_SOCK':str(node_dir/'node.sock'),'JARVIS_PROVIDER_INSTANCE_ID':work['id']})
 _,page=request('/events?since='+str(before));check('interactive shim session actually delivers its hook to the node',any(event['envelope'].get('payload',{}).get('session_id')=='shim-session' and event['envelope'].get('instanceId')==work['id'] for event in page['events']))
 code,bad=request('/launch',{'cwd':str(repo),'cmd':'true','sourceId':'not-owned'});check('unknown account cannot silently launch the default',code==400)
 code,bad=request('/usage?'+urllib.parse.urlencode({'sourceId':work['id']}));check('unsupported Codex official quota is explicit, never another account',code==200 and bool(bad.get('error')) and not bad.get('text'))
 code,bad=request('/sources/repair',{'sourceId':'not-owned','providerHome':'/','program':'/bin/sh'});check('trust repair cannot select an unregistered home or command',code==400)
 added=home/'later-profile';(added/'sessions').mkdir(parents=True);later_file=added/'sessions/later.jsonl';later_file.write_text(json.dumps({'type':'session_meta','payload':{'id':'later-id','cwd':str(repo)}})+'\n')
 (node_dir/'provider-roots.json').write_text(json.dumps({'version':1,'sources':sources+[{'agent':'codex','providerHome':str(added)}]}))
 _,catalog=request('/sources');later=next(source for source in catalog['sources'] if source['providerHome']==str(added))
 code,body=request('/file?'+urllib.parse.urlencode({'path':str(later_file)}));before=request('/hello')[1]['cursor'];out=subprocess.run([str(hook),'codex','session-start'],env=dict(env,CODEX_HOME=str(added)),input=json.dumps({'session_id':'later-id','cwd':str(repo)}),text=True,capture_output=True,timeout=5)
 _,page=request('/events?since='+str(before));check('profile added after startup immediately supports transcript reads and hook identity',code==200 and 'later-id' in body['data'] and any(event['envelope'].get('instanceId')==later['id'] for event in page['events']))
 if args.codex_bin:
  event_names=['SessionStart','SessionEnd','UserPromptSubmit','PreToolUse','PostToolUse','PermissionRequest','Stop','SubagentStart','SubagentStop'];event_args=['session-start','session-end','prompt','pre-tool','post-tool','permission','stop','subagent-start','subagent-stop']
  for profile in profiles:
   hooks={name:[{'hooks':[{'type':'command','command':' '.join("'"+part.replace("'","'\\''")+"'" for part in [str(hook),'codex',event])}]}] for name,event in zip(event_names,event_args)};hooks['Stop'].append({'hooks':[{'type':'command','command':'echo FOREIGN_FIXTURE_HOOK'}]})
   (profile/'hooks.json').write_text(json.dumps({'hooks':hooks}));(profile/'config.toml').write_text('approval_policy = "on-request"\nsandbox_mode = "workspace-write"\n')
  code,ack=request('/sources/repair',{'sourceId':work['id']});check('real Codex RPC trusts the selected remote source only',code==200 and ack.get('trusted') and 'trusted_hash' in (profiles[1]/'config.toml').read_text() and 'trusted_hash' not in (profiles[0]/'config.toml').read_text())
  config=(profiles[1]/'config.toml').read_text();check('remote trust preserves approvals and sandbox configuration','approval_policy = "on-request"' in config and 'sandbox_mode = "workspace-write"' in config)
  code,ack=request('/sources/repair',{'sourceId':work['id']});check('remote trust repair is idempotent',code==200 and ack.get('trusted') and (profiles[1]/'config.toml').read_text()==config)
 completed=True
finally:
 if shim_child and shim_child.poll() is None:
  shim_child.terminate()
  try:shim_child.wait(timeout=5)
  except subprocess.TimeoutExpired:shim_child.kill();shim_child.wait(timeout=5)
 if pty_master is not None:os.close(pty_master)
 if child and child.poll() is None:
  child.terminate()
  try:child.wait(timeout=5)
  except subprocess.TimeoutExpired:child.kill();child.wait(timeout=5)
 subprocess.run([shutil.which('tmux') or 'tmux','-L','jarvis','kill-server'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
 report={'ok':completed and bool(results) and all(row['passed'] for row in results),'scope':'Actual isolated jarvis-node, jarvis-hook and tmux; synthetic provider homes and receiver; no provider authorization or VM provisioning','fixture':str(root),'checks':results}
 destination=pathlib.Path(args.output) if args.output else root/'report.json';destination.parent.mkdir(parents=True,exist_ok=True);destination.write_text(json.dumps(report,indent=2,ensure_ascii=False)+'\n');print('Report: '+str(destination))
