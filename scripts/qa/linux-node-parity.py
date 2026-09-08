#!/usr/bin/env python3
"""Portable Linux build + real node fixture on an explicitly running SSH host.

Writes only guest /tmp/jarvis-linux-qa-* and one uniquely named transient user
unit. It never starts/stops the VM or changes agent accounts or user profiles.
Rust/Codex dependencies are scoped to the temporary fixture, with no auth.
"""
import argparse,io,json,os,pathlib,shlex,socket,subprocess,tarfile,tempfile,time,urllib.request

p=argparse.ArgumentParser();p.add_argument('--ssh-config',required=True);p.add_argument('--host',required=True);p.add_argument('--output',required=True);args=p.parse_args()
repo=pathlib.Path(__file__).resolve().parents[2]
ssh=['ssh','-F',args.ssh_config,'-o','BatchMode=yes','-o','ForwardAgent=no',args.host]
output=pathlib.Path(args.output);output.mkdir(parents=True,exist_ok=True)
def remote(script,timeout=30,data=None):
 result=subprocess.run(ssh+[script],input=data,capture_output=True,timeout=timeout)
 if result.returncode:raise RuntimeError(result.stderr.decode(errors='replace')[-6000:])
 return result.stdout.decode()
root=remote('mktemp -d /tmp/jarvis-linux-qa-XXXXXX').strip()
assert root.startswith('/tmp/jarvis-linux-qa-') and len(root.split('/'))==3
unit='jarvis-qa-'+root.rsplit('-',1)[-1].lower();tunnel=None;forward=None;results=[];completed=False
def check(name,condition,evidence=None):
 row={'scenario':name,'passed':bool(condition),'evidence':evidence};results.append(row);print(json.dumps(row),flush=True)
 if not condition:raise AssertionError(name)
def wait(fn,seconds=15):
 deadline=time.monotonic()+seconds
 while time.monotonic()<deadline:
  try:
   value=fn()
   if value:return value
  except (OSError,ValueError):pass
  time.sleep(.1)
 raise RuntimeError('Readiness timeout')
try:
 archive=io.BytesIO()
 with tarfile.open(fileobj=archive,mode='w:gz') as tar:
  files=[('portable/Cargo.toml',repo/'src-tauri/node/Cargo.toml'),('portable/src/codex_hooks.rs',repo/'src-tauri/src/codex_hooks.rs')]
  files += [('portable/src/node/'+path.name,path) for path in (repo/'src-tauri/node/src/node').glob('*.rs')]
  files += [('bin/'+name,repo/'bin'/name) for name in ['jarvis-hook','agent-shim','jarvis-tmux.conf']]
  files += [('node-parity-fixture.py',repo/'scripts/qa/node-parity-fixture.py')]
  for name,path in files:tar.add(path,arcname=name)
  main=(repo/'src-tauri/node/src/main.rs').read_text().replace('#[path = "../../src/codex_hooks.rs"]','#[path = "codex_hooks.rs"]')
  data=main.encode();info=tarfile.TarInfo('portable/src/main.rs');info.size=len(data);tar.addfile(info,io.BytesIO(data))
 remote('tar -xzf - -C '+shlex.quote(root),data=archive.getvalue())
 bootstrap=f'''set -eu
cd {shlex.quote(root)}
mkdir -p tools-home cargo rustup
export HOME={shlex.quote(root+'/tools-home')} CARGO_HOME={shlex.quote(root+'/cargo')} RUSTUP_HOME={shlex.quote(root+'/rustup')}
export npm_config_cache={shlex.quote(root+'/npm-cache')}
curl -fsSLO https://static.rust-lang.org/rustup/dist/aarch64-unknown-linux-gnu/rustup-init
curl -fsSLO https://static.rust-lang.org/rustup/dist/aarch64-unknown-linux-gnu/rustup-init.sha256
sha256sum -c rustup-init.sha256
chmod 700 rustup-init
./rustup-init -y --no-modify-path --profile minimal --default-toolchain 1.96.0
export PATH="$CARGO_HOME/bin:$PATH"
cargo test --manifest-path portable/Cargo.toml
cargo build --manifest-path portable/Cargo.toml
npm install --ignore-scripts --no-audit --no-fund --prefix {shlex.quote(root+'/codex')} @openai/codex@0.153.1
python3 node-parity-fixture.py --node portable/target/debug/jarvis-node --hook bin/jarvis-hook --codex-bin codex/node_modules/@openai/codex/bin/codex.js --output fixture-report.json
'''
 with open(output/'linux-build.log','wb') as log:
  child=subprocess.Popen(ssh+['bash -s'],stdin=subprocess.PIPE,stdout=log,stderr=subprocess.STDOUT)
  child.stdin.write(bootstrap.encode());child.stdin.close()
  deadline=time.monotonic()+900
  while child.poll() is None:
   if time.monotonic()>deadline:child.terminate();raise RuntimeError('Linux dependency/build timeout')
   time.sleep(1)
  check('portable sources build and runtime suite on Linux',child.returncode==0,{'exitCode':child.returncode})
 report=json.loads(remote('cat '+shlex.quote(root+'/fixture-report.json')));(output/'node-fixture.json').write_text(json.dumps(report,indent=2,ensure_ascii=False)+'\n')
 check('Linux runtime checks all passed',report['ok'],{'checks':len(report['checks'])})
 home=root+'/service-home';data_dir=root+'/service-node'
 remote('mkdir -p '+shlex.quote(home)+' '+shlex.quote(data_dir))
 command=['systemd-run','--user','--unit='+unit,'--collect','-p','Restart=on-failure','--setenv=HOME='+home,'--setenv=JARVIS_DIR='+data_dir,root+'/portable/target/debug/jarvis-node']
 remote(shlex.join(command))
 with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
 forward=f'127.0.0.1:{port}:{data_dir}/node.sock'
 with open(output/'tunnel.log','wb') as log:tunnel=subprocess.Popen(ssh[:-1]+['-N','-o','ControlMaster=no','-o','ControlPath=none','-o','ForkAfterAuthentication=no','-o','ExitOnForwardFailure=yes','-L',forward,args.host],stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT)
 def get(path):
  with urllib.request.urlopen(f'http://127.0.0.1:{port}'+path,timeout=5) as response:return json.load(response)
 hello=wait(lambda:get('/hello'))
 check('Jarvis owns a foreground tunnel despite Lima ControlMaster configuration',tunnel.poll() is None)
 check('Lima provided SSH config forwards native VM node socket',hello['protocol']==2 and 'sources.trustRepair' in hello['capabilities'])
 remote('systemctl --user restart '+shlex.quote(unit))
 restarted=wait(lambda:(value if (value:=get('/hello'))['instance']!=hello['instance'] else None))
 check('systemd restart exposes a new replay epoch over the same SSH tunnel',restarted['instance']!=hello['instance'])
 completed=True
finally:
 # Lima uses a ControlMaster; `ssh -N` may exit after handing the forward to
 # it. Cancel that exact mapping instead of assuming the child owns its life.
 if forward:
  subprocess.run(ssh[:-1]+['-O','cancel','-L',forward,args.host],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=10)
 if tunnel and tunnel.poll() is None:
  tunnel.terminate()
  try:tunnel.wait(timeout=5)
  except subprocess.TimeoutExpired:tunnel.kill();tunnel.wait(timeout=5)
 try:remote('systemctl --user stop '+shlex.quote(unit)+' 2>/dev/null || true')
 except Exception:pass
 (output/'report.json').write_text(json.dumps({'ok':completed and all(row['passed'] for row in results),'guestFixture':root,'unit':unit,'checks':results},indent=2)+'\n')
 print('Report: '+str(output/'report.json'),flush=True)
