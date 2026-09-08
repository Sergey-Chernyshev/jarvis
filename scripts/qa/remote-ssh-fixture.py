#!/usr/bin/env python3
"""Real localhost SSH / tmux / node integration, with synthetic data only.

Run with an existing jarvis-node binary. Requires OpenSSH server/client, tmux,
Python 3, and permission to bind localhost. Never changes ~/.ssh or CLI auth.
All daemons and tmux panes have an isolated temporary profile and are stopped.
Temporary logs/results are retained for inspection (no provider credentials).
"""
import getpass
import json
import os
import pathlib
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import urllib.parse


binary = pathlib.Path(sys.argv[1]).resolve(strict=True)
root = pathlib.Path(tempfile.mkdtemp(prefix='jarvis-remote-qa-', dir='/tmp')).resolve()
root.chmod(0o700)
tmux = shutil.which('tmux')
sshd = shutil.which('sshd') or '/usr/sbin/sshd'
assert tmux and pathlib.Path(sshd).is_file(), 'OpenSSH server and tmux required'
repo = root / 'repo'
repo.mkdir()
(root / 'node-data').mkdir()
(root / 'tmux-runtime').mkdir()
(repo / 'receiver.py').write_text('import sys,json\nfor line in sys.stdin:\n'
    ' with open("received.jsonl","a") as f: f.write(json.dumps(line)+"\\n")\n'
    ' print("QA_RECEIVED",flush=True)\n')
env = dict(os.environ, TMUX_TMPDIR=str(root / 'tmux-runtime'), LC_ALL='C')
env.pop('TMUX', None)
processes = []
node_pid = None
results = []


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def stop(child):
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=5)


def start(args, name):
    with open(root / (name + '.log'), 'ab') as log:
        child = subprocess.Popen(args, cwd=repo, env=env, stdin=subprocess.DEVNULL,
                                 stdout=log, stderr=subprocess.STDOUT)
    processes.append(child)
    return child


def check(name, passed, evidence):
    result = {'scenario': name, 'passed': bool(passed), 'evidence': evidence}
    results.append(result)
    print(json.dumps(result, ensure_ascii=False), flush=True)


def request(path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(base + path, data=data,
                                 headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=30) as response:
        raw = response.read()
        return json.loads(raw) if raw else {}


def ready(child, probe, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if child.poll() is not None:
            raise RuntimeError('Fixture process exited; inspect logs in ' + str(root))
        try:
            if probe():
                return
        except (OSError, ValueError):
            pass
        time.sleep(.1)
    raise RuntimeError('Fixture did not become ready: ' + str(root))


try:
    for name in ('client_key', 'host_key'):
        subprocess.run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(root / name)], check=True)
    authorized = root / 'authorized_keys'
    authorized.write_bytes((root / 'client_key.pub').read_bytes())
    authorized.chmod(0o600)
    ssh_port, local_port = free_port(), free_port()
    public = (root / 'host_key.pub').read_text().split()
    (root / 'known_hosts').write_text('[127.0.0.1]:' + str(ssh_port) + ' ' + ' '.join(public[:2]) + '\n')
    (root / 'sshd_config').write_text(f'''Port {ssh_port}
ListenAddress 127.0.0.1
HostKey {root}/host_key
PidFile {root}/sshd.pid
AuthorizedKeysFile {root}/authorized_keys
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
UsePAM no
StrictModes no
AllowUsers {getpass.getuser()}
LogLevel ERROR
''')
    (root / 'ssh_config').write_text(f'''Host jarvis-qa
 HostName 127.0.0.1
 Port {ssh_port}
 User {getpass.getuser()}
 IdentityFile {root}/client_key
 IdentitiesOnly yes
 UserKnownHostsFile {root}/known_hosts
 StrictHostKeyChecking yes
 BatchMode yes
 ControlMaster no
 LogLevel ERROR
''')
    ssh = ['/usr/bin/ssh', '-F', str(root / 'ssh_config')]
    daemon = start([sshd, '-D', '-e', '-f', str(root / 'sshd_config')], 'sshd')
    def ssh_ready():
        out = subprocess.run(ssh + ['jarvis-qa', 'printf JARVIS_SSH_OK'], capture_output=True, timeout=3)
        return out.returncode == 0 and out.stdout == b'JARVIS_SSH_OK'
    ready(daemon, ssh_ready)
    # Keep this server alive when tested panes exit; neither sockets nor config
    # are shared with the user's normal tmux instance.
    subprocess.run([tmux, '-u', '-f', '/dev/null', '-L', 'jarvis', 'new-session',
                    '-d', '-s', 'qa-keepalive', '-c', str(repo), 'sleep 1800'],
                   env=env, cwd=repo, check=True)
    node_env = ['env', 'JARVIS_DIR=' + str(root / 'node-data'),
                'TMUX_TMPDIR=' + str(root / 'tmux-runtime'), 'JARVIS_NODE_BUFFER=8',
                'LC_ALL=C', 'PATH=' + str(pathlib.Path(tmux).parent) + ':/usr/bin:/bin', str(binary)]
    sock = root / 'node-data/node.sock'
    def start_node():
        pidfile = root / 'node.pid'
        pidfile.unlink(missing_ok=True)
        cmd = 'echo $$ > ' + shlex.quote(str(pidfile)) + '; exec ' + shlex.join(node_env)
        child = start(ssh + ['jarvis-qa', cmd], 'node')
        ready(child, lambda: sock.exists() and pidfile.exists())
        return child, int(pidfile.read_text())
    node, node_pid = start_node()
    forward_args = ssh + ['-N', '-o', 'ExitOnForwardFailure=yes',
                          '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=3',
                          '-L', f'127.0.0.1:{local_port}:{sock}', 'jarvis-qa']
    base = f'http://127.0.0.1:{local_port}'
    tunnel = start(forward_args, 'tunnel')
    ready(tunnel, lambda: request('/hello').get('node') == 'jarvis-node')
    probe = subprocess.run([sys.executable, str(pathlib.Path(__file__).with_name('remote-protocol-probe.py')),
                            str(root), base], check=False)
    check('protocol probe subprocess', probe.returncode == 0, {'exitCode': probe.returncode})
    # A duplicate process must fail without stealing the live Unix socket.
    inode = sock.stat().st_ino
    duplicate = subprocess.run(ssh + ['jarvis-qa', shlex.join(node_env)],
                               capture_output=True, timeout=10)
    check('duplicate node cannot replace live socket', duplicate.returncode != 0
          and sock.stat().st_ino == inode and request('/hello')['node'] == 'jarvis-node',
          {'duplicateExitCode': duplicate.returncode, 'socketInodeUnchanged': sock.stat().st_ino == inode})
    cursor = request('/hello')['cursor']
    stop(tunnel)
    # Events are written via the real SSH connection directly to node.sock
    # while the laptop's TCP forward is disconnected.
    for i in range(3):
        payload = json.dumps({'event': 'post-tool', 'agent': 'codex', 'payload': {'qa': i}})
        subprocess.run(ssh + ['jarvis-qa', 'curl -fsS --unix-socket ' + shlex.quote(str(sock))
            + ' -H "Content-Type: application/json" -d ' + shlex.quote(payload) + ' http://localhost/event'],
            capture_output=True, check=True, timeout=5)
    tunnel = start(forward_args, 'tunnel')
    ready(tunnel, lambda: request('/hello').get('node') == 'jarvis-node')
    page = request('/events?since=' + str(cursor))
    check('reconnect replays events in exact order', [event['envelope']['payload']['qa']
          for event in page.get('events', [])] == [0, 1, 2]
          and page['cursor'] == cursor + 3, page)
    cursor = page['cursor']
    for i in range(10):
        request('/event', {'event': 'stop', 'payload': {'qa': i}})
    gap = request('/events?since=' + str(cursor))
    check('buffer overflow produces explicit gap', gap.get('gap') is True
          and gap['cursor'] == cursor + 10, gap)
    cursor = gap['cursor']
    old_instance = request('/hello')['instance']
    # Test a real node crash/restart, keeping the SSH forward alive.
    os.kill(node_pid, signal.SIGKILL)
    node.wait(timeout=5)
    node_pid = None
    node, node_pid = start_node()
    ready(tunnel, lambda: request('/hello').get('cursor') == 0)
    gap = request('/events?since=' + str(cursor))
    check('node restart invalidates previous cursor', gap.get('gap') is True
          and gap.get('cursor') == 0, gap)
    request('/event', {'event': 'prompt', 'payload': {'qa': 'after-restart'}})
    page = request('/events?since=0')
    check('stream resumes after node restart gap', page.get('cursor') == 1
          and page['events'][0]['envelope']['payload']['qa'] == 'after-restart', page)
    for i in range(3):
        request('/event', {'event': 'post-tool', 'payload': {'qa': i}})
    gap = request('/events?' + urllib.parse.urlencode({'since': 1, 'instance': old_instance}))
    check('restart detected even after numeric cursor catches up', gap.get('gap') is True
          and gap.get('cursor') == 0 and gap.get('instance') != old_instance, gap)
finally:
    if node_pid is not None:
        try:
            os.kill(node_pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    for child in reversed(processes):
        stop(child)
    subprocess.run([tmux, '-u', '-L', 'jarvis', 'kill-server'], env=env,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
    (root / 'lifecycle-results.json').write_text(json.dumps(results, ensure_ascii=False, indent=2))
    print('Fixture evidence: ' + str(root), flush=True)

sys.exit(0 if results and all(result['passed'] for result in results) else 1)
