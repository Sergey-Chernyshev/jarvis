#!/usr/bin/env python3
"""Build and probe jarvis-node inside an already-running Lima Linux VM.

Only project source and a synthetic receiver enter a private /tmp directory.
No CLI authentication, hooks, existing worktrees, or user tmux server is used.
The caller owns the VM lifecycle; this script stops only its own node/tmux.
"""
import argparse, io, json, os, pathlib, subprocess, tarfile
parser = argparse.ArgumentParser()
parser.add_argument('--vm', required=True)
parser.add_argument('--lima-home', required=True)
args = parser.parse_args()
repo = pathlib.Path(__file__).resolve().parents[2]
env = dict(os.environ, LIMA_HOME=args.lima_home)
base = ['limactl', 'shell', '--workdir', '/tmp', args.vm]
def guest(argv, **kw):
    return subprocess.run(base + argv, env=env, check=True, **kw)
root = guest(['mktemp', '-d', '/tmp/jarvis-remote-qa-XXXXXXXX'], capture_output=True, text=True).stdout.strip()
if not root.startswith('/tmp/jarvis-remote-qa-') or '\n' in root:
    raise RuntimeError('Invalid fixture directory')
print('Linux VM fixture: ' + root, flush=True)
archive = io.BytesIO()
with tarfile.open(fileobj=archive, mode='w:gz') as tar:
    tar.add(repo/'src-tauri/node/Cargo.toml', arcname='node/Cargo.toml')
    tar.add(repo/'src-tauri/node/src', arcname='node/src')
    tar.add(repo/'src-tauri/Cargo.lock', arcname='node/Cargo.lock')
    tar.add(repo/'scripts/qa/remote-protocol-probe.py', arcname='probe.py')
guest(['tar', '-xz', '-C', root], input=archive.getvalue())
# The parent workspace lock contains extra packages: prune it offline first.
guest(['cargo', 'generate-lockfile', '--offline', '--manifest-path', root+'/node/Cargo.toml'], timeout=60)
guest(['cargo', 'test', '--locked', '--manifest-path', root+'/node/Cargo.toml'], timeout=360)
guest(['cargo', 'build', '--locked', '--manifest-path', root+'/node/Cargo.toml'], timeout=180)
runner = r'''
import json, os, pathlib, socket, subprocess, sys, time, urllib.request
root = pathlib.Path(sys.argv[1]); repo = root/'repo'; repo.mkdir()
(root/'tmux-runtime').mkdir(); (root/'node-data').mkdir()
(repo/'receiver.py').write_text('import sys,json\nfor line in sys.stdin:\n with open("received.jsonl","a") as f: f.write(json.dumps(line)+"\\n")\n print("QA_RECEIVED",flush=True)\n')
with socket.socket() as sock:
    sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
env=dict(os.environ, JARVIS_DIR=str(root/'node-data'), TMUX_TMPDIR=str(root/'tmux-runtime'), JARVIS_NODE_TCP='127.0.0.1:'+str(port), LC_ALL='C')
env.pop('TMUX',None)
with open(root/'node.log','wb') as log:
    child=subprocess.Popen([str(root/'node/target/debug/jarvis-node')],env=env,cwd=repo,stdout=log,stderr=subprocess.STDOUT)
try:
    for _ in range(100):
        try:
            with urllib.request.urlopen('http://127.0.0.1:'+str(port)+'/hello',timeout=.5) as res:
                if json.load(res).get('node')=='jarvis-node': break
        except OSError: time.sleep(.1)
    else: raise RuntimeError('Node did not become ready')
    subprocess.run([sys.executable,str(root/'probe.py'),str(root),'http://127.0.0.1:'+str(port)],env=env,check=True,timeout=90)
finally:
    if child.poll() is None:
        child.terminate()
        try: child.wait(timeout=5)
        except subprocess.TimeoutExpired: child.kill(); child.wait()
    subprocess.run(['tmux','-L','jarvis','kill-server'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
if (root/'node-data/node.sock').exists(): raise RuntimeError('SIGTERM left the socket behind')
print('LINUX_VM_PROTOCOL_PASS')
'''
guest(['python3', '-c', runner, root], timeout=120)
report = guest(['cat', root+'/protocol-results.json'], capture_output=True, text=True).stdout
out = repo/'docs/qa/assets/linux-vm'; out.mkdir(parents=True,exist_ok=True)
(out/'protocol-results.json').write_text(report)
(out/'environment.json').write_text(json.dumps({'vm':args.vm,'fixture':root,'scope':'Actual Linux node build/unit suite/tmux/HTTP over loopback inside Lima; SSH commands originate on macOS. Synthetic payloads only.'},indent=2))
