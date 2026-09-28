import test from 'node:test';
import assert from 'node:assert/strict';
import {createCloudMachineLinker,CLOUD_MACHINE_LINK_ERROR_CODES} from './cloud-identity-custody.mjs';
const hex=(text,width=64)=>[...text].map(c=>c.charCodeAt(0).toString(16)).join('').padEnd(width,'0').slice(0,width);
// Machine B's scope sorts before machine A's, so lock order is observable.
const SCOPES={'m-a':'f'.repeat(64),'m-b':'1'.repeat(64)};
const encode=value=>new TextEncoder().encode(JSON.stringify(value));
function fixture({step='enrolled'}={}){
 const held=new Set(),order=[],rows=new Map(),index=new Map(),writes=[],calls=[];
 const locks={async request(name,options,operation){assert.equal(options.ifAvailable,true);if(held.has(name))return operation(null);held.add(name);order.push(name);try{return await operation({});}finally{held.delete(name);}}};
 const store={candidates:async locator=>[...(index.get(locator)??[])],load:async key=>rows.has(key)?rows.get(key).slice():undefined,save:async()=>{writes.push('save');},remember:async()=>{writes.push('remember');}};
 const loaded=[];const load=store.load;store.load=async key=>{const value=await load(key);if(value)loaded.push(value);return value;};
 const runtime={
  cloud_identity_scope:(account,machine)=>SCOPES[machine]??hex(machine),
  cloud_identity_create:()=>{throw Error('linking must never create an identity');},
  cloud_identity_public:(bytes,account,machine)=>{const v=JSON.parse(new TextDecoder().decode(bytes));if(v.account!==account||v.machine!==machine)throw Error('Cloud owner enrollment refused (Scope)');return JSON.stringify({scope:SCOPES[machine]??hex(machine),device_public_key:v.device,binding:'b'.repeat(64),step:v.step});},
  cloud_mesh_link:(...args)=>{calls.push(['link',...args.map(v=>ArrayBuffer.isView(v)?JSON.parse(new TextDecoder().decode(v)).machine:v)]);return JSON.stringify({version:1,kind:'link',link_id:args[7],source_signature:[1],target_signature:[2]});},
  cloud_mesh_unlink:(...args)=>{calls.push(['unlink',...args.map(v=>ArrayBuffer.isView(v)?JSON.parse(new TextDecoder().decode(v)).machine:v)]);return JSON.stringify({version:1,kind:'unlink'});},
 };
 const enroll=(machine,device,value=step)=>{const locator=`tilo-certified-cloud-v4:${SCOPES[machine]??hex(machine)}`;rows.set(device,encode({account:'acct',machine,device,step:value}));index.set(locator,[...(index.get(locator)??[]),device]);};
 const session={getItem:()=>null,setItem:()=>writes.push('session')};
 const sign=createCloudMachineLinker({runtime,locks,store,session});
 return {held,order,rows,writes,calls,loaded,runtime,enroll,sign,locks};
}
const DA='a'.repeat(64),DB='c'.repeat(64);
const link={accountId:'acct',kind:'link',sourceMachineId:'m-a',targetMachineId:'m-b',linkId:'link-1',sourceChallenge:'{"machine_id":"m-a"}',targetChallenge:'{"machine_id":"m-b"}'};
const zeroed=value=>value.every(byte=>byte===0);

test('link signs with both stored enrolled identities under sorted custody locks, then wipes and releases',async()=>{
 const f=fixture();f.enroll('m-a',DA);f.enroll('m-b',DB);
 const payload=await f.sign(link);
 assert.equal(JSON.parse(payload).link_id,'link-1');
 assert.deepEqual(f.calls,[['link','acct','m-a','m-a',link.sourceChallenge,'m-b','m-b',link.targetChallenge,'link-1']]);
 assert.deepEqual(f.order,[`tilo-cloud-scope:${SCOPES['m-b']}`,`tilo-cloud-scope:${SCOPES['m-a']}`,`tilo-certified-device:${DA}`,`tilo-certified-device:${DB}`]);
 assert.equal(f.held.size,0);assert.deepEqual(f.writes,[]);
 assert.ok(f.loaded.length===2&&f.loaded.every(zeroed));
 assert.ok(!/secret|material|private/i.test(payload));
});
test('a machine without a stored identity is identity-missing and nothing is created',async()=>{
 const f=fixture();f.enroll('m-a',DA);
 await assert.rejects(f.sign(link),error=>error.code==='identity-missing'&&error.machineId==='m-b'&&/Open each machine's workspace/.test(error.message));
 assert.equal(f.held.size,0);assert.deepEqual(f.writes,[]);assert.deepEqual(f.calls,[]);assert.ok(f.loaded.every(zeroed));
});
test('an identity that never finished enrollment cannot approve a link',async()=>{
 const f=fixture();f.enroll('m-a',DA);f.enroll('m-b',DB,'apply');
 await assert.rejects(f.sign(link),error=>error.code==='identity-missing'&&error.machineId==='m-b');
 assert.deepEqual(f.calls,[]);assert.equal(f.held.size,0);
});
test('an open workspace holding a machine scope or device fails fast as workspace-open',async()=>{
 const f=fixture();f.enroll('m-a',DA);f.enroll('m-b',DB);
 let release;const open=f.locks.request(`tilo-cloud-scope:${SCOPES['m-a']}`,{ifAvailable:true},()=>new Promise(yes=>release=yes));
 await assert.rejects(f.sign(link),error=>error.code==='workspace-open'&&error.machineId==='m-a');
 assert.deepEqual(f.loaded,[]);release();await open;assert.equal(f.held.size,0);
 const g=fixture();g.enroll('m-a',DA);g.enroll('m-b',DB);
 let unlock;const device=g.locks.request(`tilo-certified-device:${DB}`,{ifAvailable:true},()=>new Promise(yes=>unlock=yes));
 await assert.rejects(g.sign(link),error=>error.code==='workspace-open'&&error.machineId==='m-b');
 unlock();await device;assert.equal(g.held.size,0);assert.deepEqual(g.calls,[]);
});
test('unlink follows the grant orientation whichever order the machines are given in',async()=>{
 const f=fixture();f.enroll('m-a',DA);f.enroll('m-b',DB);
 const grant=JSON.stringify({version:1,kind:'link',link_id:'l',source:{machine_id:'m-b',expires_at_ns:1758888888888888888},target:{machine_id:'m-a'}});
 await f.sign({accountId:'acct',kind:'unlink',sourceMachineId:'m-a',targetMachineId:'m-b',grant});
 assert.deepEqual(f.calls,[['unlink','acct','m-b','m-b','m-a','m-a',grant]]);
 await assert.rejects(f.sign({accountId:'acct',kind:'unlink',sourceMachineId:'m-a',targetMachineId:'m-c',grant}),TypeError);
 assert.equal(f.held.size,0);
});
test('abort and native refusals release custody and wipe loaded material',async()=>{
 const f=fixture();f.enroll('m-a',DA);f.enroll('m-b',DB);const controller=new AbortController();
 const load=f.rows.get(DA);f.rows.set(DA,load);const original=f.runtime.cloud_identity_public;
 f.runtime.cloud_identity_public=(...args)=>{controller.abort(Error('stop'));return original(...args);};
 await assert.rejects(f.sign({...link,signal:controller.signal}),/stop/);
 assert.equal(f.held.size,0);assert.ok(f.loaded.every(zeroed));assert.deepEqual(f.calls,[]);
 const g=fixture();g.enroll('m-a',DA);g.enroll('m-b',DB);g.runtime.cloud_mesh_link=()=>{throw 'Native mesh challenge belongs to different machines';};
 await assert.rejects(g.sign(link),error=>error.code==='link-refused');
 assert.equal(g.held.size,0);assert.ok(g.loaded.length===2&&g.loaded.every(zeroed));
 const h=fixture();h.enroll('m-a',DA);h.enroll('m-b',DB);h.runtime.cloud_mesh_link=()=>{throw 'Cloud owner identity is not enrolled for this machine';};
 await assert.rejects(h.sign(link),error=>error.code==='identity-missing');
 const aborted=AbortSignal.abort(Error('early'));await assert.rejects(fixture().sign({...link,signal:aborted}),/early/);
});
test('invalid requests are refused before any custody is taken',async()=>{
 const f=fixture();
 for(const bad of [undefined,{...link,kind:'merge'},{...link,accountId:''},{...link,targetMachineId:'m-a'},{...link,linkId:'a b'},{...link,sourceChallenge:''},{...link,targetChallenge:'x'.repeat(20*1024+1)},{accountId:'acct',kind:'unlink',sourceMachineId:'m-a',targetMachineId:'m-b',grant:'{'}])
  await assert.rejects(f.sign(bad),TypeError);
 assert.deepEqual(f.order,[]);
 assert.deepEqual([...CLOUD_MACHINE_LINK_ERROR_CODES],['identity-missing','workspace-open','link-refused']);
});
