#!/usr/bin/env node
// Actual stdio MCP → actual Tauri daemon/gate, in the native smoke profile.
// No real provider requests, user tokens, recording, or allowed mutations.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, writeFile, readFile, access, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { randomBytes } from 'node:crypto';
import { createInterface } from 'node:readline';
const [app, mcpBinary] = process.argv.slice(2);
if (!app || !mcpBinary) throw Error('Usage: native-mcp.mjs /absolute/debug/Jarvis.app /absolute/jarvis-mcp');
const scratch = await mkdtemp(join(tmpdir(), 'jarvis-mcp-qa-'));
const scenario = join(scratch, 'scenario.js');
await writeFile(scenario, `await t.step('Actual MCP client completes allowed and denied calls', async () => {
  await t.waitFor(async () => (await window.jarvis.getSettings()).nativeMcpQaComplete, 40000);
});`);
const smoke = spawn(process.execPath, ['scripts/native-smoke.mjs', '--app', resolve(app), '--scenario', scenario, '--timeout', '55'], { stdio: ['ignore','pipe','pipe'] });
let finished = false;
process.on('exit', () => { if (!finished) smoke.kill('SIGTERM'); });
const output = [], errors = []; let profile;
const exited = new Promise(resolve => smoke.on('exit', (code, signal) => resolve({code,signal})));
smoke.stdout.on('data', chunk => { output.push(chunk); const found = Buffer.concat(output).toString().match(/profile ([^\n]+)/); if(found) profile=found[1]; });
smoke.stderr.on('data', chunk => errors.push(chunk));
const deadline = Date.now()+30000;
while (!profile) { if(Date.now()>deadline || smoke.exitCode!==null) throw Error('Native profile unavailable'); await new Promise(r=>setTimeout(r,40)); }
const data = join(profile, 'data'), socket = join(data, 'run.sock');
while (true) { try { await access(socket); break; } catch { if(Date.now()>deadline) throw Error('Native socket unavailable'); await new Promise(r=>setTimeout(r,40)); } }
const token = randomBytes(32).toString('hex');
await writeFile(join(data,'tokens.json'),JSON.stringify({agent:token}),{mode:0o600});
const records=[]; let child;
function client(tokenValue) {
  const env={...process.env,JARVIS_SOCK:socket,JARVIS_DIR:data}; delete env.JARVIS_TOKEN;
  if(tokenValue) env.JARVIS_TOKEN=tokenValue;
  const proc=spawn(resolve(mcpBinary),[],{env,stdio:['pipe','pipe','pipe']});
  const lines=createInterface({input:proc.stdout}); const pending=new Map(); let sequence=0;
  lines.on('line',line=>{const response=JSON.parse(line);pending.get(response.id)?.(response);pending.delete(response.id);});
  proc.stderr.resume();
  return {proc,call(method,params={}) {const id=++sequence; return new Promise((accept,reject)=>{
    const timeout=setTimeout(()=>{pending.delete(id);reject(Error('MCP request timed out: '+method));},7000);
    pending.set(id,response=>{clearTimeout(timeout);accept(response.result || response);});
    proc.stdin.write(JSON.stringify({jsonrpc:'2.0',id,method,params})+'\n');
  });}};
}
let failure;
try {
  child=client(token);
  const init=await child.call('initialize',{protocolVersion:'2024-11-05',capabilities:{},clientInfo:{name:'jarvis-native-qa',version:'1'}});
  assert.equal(init.serverInfo.name,'jarvis'); records.push({name:'actual stdio initialize',ok:true});
  const listed=await child.call('tools/list');
  assert.ok(listed.tools.some(tool=>tool.name==='meetings.list')); assert.ok(listed.tools.some(tool=>tool.name==='meetings.get'));
  assert.ok(!listed.tools.some(tool=>['meetings.start','audit.query'].includes(tool.name)));
  records.push({name:'actual registry and agent grant filter',ok:true,tools:listed.tools.length});
  const meetingList=await child.call('tools/call',{name:'meetings.list',arguments:{limit:10}});
  assert.equal(meetingList.isError,false); assert.deepEqual(meetingList.structuredContent.value.meetings,[]);
  assert.equal(meetingList.structuredContent.provenance,'untrusted');
  records.push({name:'actual meeting archive read and provenance',ok:true});
  const missing=await child.call('tools/call',{name:'meetings.get',arguments:{id:'qa-absent-meeting'}});
  assert.equal(missing.isError,true); assert.ok(missing.structuredContent.error);
  records.push({name:'missing meeting returns a tool error',ok:true});
  const invalid=await child.call('tools/call',{name:'meetings.list',arguments:{limit:0}});
  assert.equal(invalid.isError,true); records.push({name:'invalid meeting input rejected',ok:true});
  const denied=await child.call('tools/call',{name:'settings.set',arguments:{patch:{launchDangerous:true}}});
  assert.equal(denied.isError,true); assert.match(denied.structuredContent.error,/allowlist|защищ|грант/);
  assert.notEqual(JSON.parse(await readFile(join(data,'settings.json'),'utf8')).launchDangerous,true);
  records.push({name:'actual settings gate denies non-allowlisted mutation without writing',ok:true});
  child.proc.stdin.end(); child=client(null);
  const unauthorized=await child.call('tools/call',{name:'meetings.list',arguments:{}});
  assert.equal(unauthorized.isError,true); assert.match(unauthorized.structuredContent.error,/токен/);
  records.push({name:'missing caller token rejected by actual daemon',ok:true});
} catch(error) {failure=error.stack;process.exitCode=1;}
finally {
  child?.proc.stdin.end();
  const settings=JSON.parse(await readFile(join(data,'settings.json'),'utf8')); settings.nativeMcpQaComplete=true;
  await writeFile(join(data,'settings.json'),JSON.stringify(settings),{mode:0o600});
}
const outcome=await exited;
finished = true;
const out=resolve('docs/qa/assets/native-mcp');await mkdir(out,{recursive:true});
const report={ok:!failure&&outcome.code===0,scope:'Actual debug Tauri daemon, MCP stdio, capability registry and gate in isolated profile; empty meeting archive. No recording, provider request or authorized mutation.',records,failure,outcome,profile};
await writeFile(join(out,'report.json'),JSON.stringify(report,null,2));
await writeFile(join(scratch,'native-smoke.log'),Buffer.concat(output));
console.log(JSON.stringify(report,null,2));if(!report.ok)process.exitCode=1;
