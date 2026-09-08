#!/usr/bin/env python3
"""Probe an already running disposable localhost Jarvis node via real SSH.

Only use a temporary directory containing repo/receiver.py and an isolated tmux
server. This intentionally sends synthetic content, never workspace files.
"""
import concurrent.futures
import json
import pathlib
import shlex
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


root = pathlib.Path(sys.argv[1]).resolve()
base = sys.argv[2]
if root.parent != pathlib.Path('/tmp').resolve() or not root.name.startswith('jarvis-remote-qa-'):
    raise SystemExit('Expected a disposable Jarvis QA directory')
if urllib.parse.urlparse(base).hostname != '127.0.0.1':
    raise SystemExit('Only a localhost SSH forward is supported')
results = []


def request(path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(base + path, data=data,
                                 headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=35) as response:
            raw = response.read().decode()
            try:
                value = json.loads(raw)
            except json.JSONDecodeError:
                value = {'text': raw}
            return response.status, value
    except urllib.error.HTTPError as error:
        return error.code, json.load(error)


def check(name, condition, evidence):
    results.append({'scenario': name, 'passed': bool(condition), 'evidence': evidence})


code, hello = request('/hello')
check('SSH TCP-to-Unix handshake', code == 200 and hello.get('node') == 'jarvis-node', hello)
cursor = hello['cursor']
with concurrent.futures.ThreadPoolExecutor() as pool:
    pending = pool.submit(request, '/events?since=' + str(cursor))
    time.sleep(.2)
    sent = {'event': 'prompt', 'agent': 'claude', 'payload': {
        'session_id': 'qa-synthetic', 'turn_id': 'qa-turn', 'prompt': 'Привет 👋'}}
    code, ack = request('/event', sent)
    code, page = pending.result()
check('long-poll wakes on exact hook payload', code == 200 and
      any(event['envelope'] == sent for event in page.get('events', [])), page)

repo = root / 'repo'
code, launched = request('/launch', {'cwd': str(repo), 'cmd':
    shlex.quote(sys.executable) + ' -u receiver.py',
    'name': 'qa-receiver'})
check('launch actual process in isolated tmux', code == 200 and bool(launched.get('pane')), launched)
pane = launched.get('pane')
if pane:
    text = 'Привет 👋 literal $(touch SHOULD_NOT_EXIST) `whoami`'
    code, response = request('/reply', {'pane': pane, 'text': text})
    time.sleep(.3)
    received_path = repo / 'received.jsonl'
    received = [json.loads(line).rstrip('\n') for line in received_path.read_text().splitlines()] if received_path.exists() else []
    check('reply preserves Unicode and literal shell syntax', code == 200 and text in received
          and not (repo / 'SHOULD_NOT_EXIST').exists(), {'response': response, 'received': received})
    code, screen = request('/screen?' + urllib.parse.urlencode({'pane': pane}))
    check('terminal screen shows the selected live pane', code == 200
          and screen.get('pane') == pane and 'QA_RECEIVED' in screen.get('screen', '')
          and not screen.get('error'), screen)
    code, key_ack = request('/keys', {'pane': pane, 'keys': [{'key': 'Enter'}]})
    check('terminal key receives explicit acknowledgement', code == 200
          and key_ack.get('ok') is True, key_ack)
    transcript = repo / 'synthetic.jsonl'
    expected = '{"type":"user","message":{"content":"Привет 👋"}}\n'
    transcript.write_text(expected)
    code, chunk = request('/file?' + urllib.parse.urlencode({'path': str(transcript), 'from': 0}))
    check('read fixture transcript under live pane cwd', code == 200 and chunk.get('data') == expected, chunk)
    encoded = '👋\n'.encode()
    transcript.write_bytes(encoded[:2])
    code, partial = request('/file?' + urllib.parse.urlencode({'path': str(transcript), 'from': 0}))
    transcript.write_bytes(encoded)
    code, completed = request('/file?' + urllib.parse.urlencode({'path': str(transcript), 'from': partial.get('next', 0)}))
    check('tail waits for a split UTF-8 character', partial.get('next') == 0
          and partial.get('data') == '' and completed.get('data') == '👋\n',
          {'partial': partial, 'completed': completed})
    transcript.write_text('{}\n')
    code, rewound = request('/file?' + urllib.parse.urlencode({'path': str(transcript), 'from': 999}))
    check('truncated transcript reports rewind cursor', code == 200 and
          rewound.get('from') == 3 and rewound.get('next') == 3, rewound)
    code, refused = request('/file?' + urllib.parse.urlencode({'path': '/etc/passwd', 'from': 0}))
    check('reject file outside allowed roots', code == 403, {'http': code, 'response': refused})

with concurrent.futures.ThreadPoolExecutor() as pool:
    futures = [pool.submit(request, '/launch', {'cwd': str(repo), 'cmd': 'sleep 60',
               'name': 'qa-same-name'}) for _ in range(2)]
    outcomes = [future.result() for future in futures]
check('concurrent same-name launches are independent', all(code == 200 for code, _ in outcomes)
      and len({body.get('pane') for _, body in outcomes}) == 2, outcomes)
for code, body in outcomes:
    if code == 200 and body.get('pane'):
        request('/kill', {'pane': body['pane']})
if pane:
    request('/kill', {'pane': pane})
    code, gone = request('/screen?' + urllib.parse.urlencode({'pane': pane}))
    check('ended terminal reports its pane error', code == 200 and bool(gone.get('error'))
          and gone.get('pane') == pane, gone)

print(json.dumps(results, ensure_ascii=False, indent=2))
(root / 'protocol-results.json').write_text(json.dumps(results, ensure_ascii=False, indent=2))
sys.exit(0 if all(result['passed'] for result in results) else 1)
