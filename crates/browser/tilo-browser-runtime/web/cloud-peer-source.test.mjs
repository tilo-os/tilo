import test from 'node:test';import assert from 'node:assert/strict';
import {createCloudPeerSource} from './cloud-peer-source.mjs';
const deferred=()=>{let resolve;const promise=new Promise(yes=>resolve=yes);return{promise,resolve};};
const namespace={profile:'certified:r',device:'d',room:'r'};
// Rust stand-in: phases advance only on the exact ACKed checkpoint bytes.
function fixture({phase='inspect'}={}){
 const calls=[];let next=0,staged=null;const state={phase};
 const runtime={
  cloud_begin(material,account,machine,existing){calls.push(['begin',[...material],account,machine,existing]);return ++next;},
  cloud_step(){calls.push(['step',state.phase]);return JSON.stringify(state.phase==='apply'?{step:'apply',operationKey:'op-1',payload:'{"signed":1}'}:{step:state.phase});},
  cloud_enroll(_id,challenge){calls.push(['enroll',challenge]);staged={bytes:[2],phase:'apply'};return JSON.stringify({kind:'checkpoint',material:[2],namespace:null});},
  cloud_accept(_id,reply){calls.push(['accept',reply]);staged={bytes:[3],phase:'enrolled'};return JSON.stringify({kind:'checkpoint',material:[3],namespace:null});},
  cloud_acknowledge(_id,material){calls.push(['ack',[...material]]);if(!staged||staged.bytes.join()!==[...material].join())throw Error('ACK mismatch');state.phase=staged.phase;staged=null;},
  async cloud_complete(_id,material){calls.push(['complete',[...material]]);return JSON.stringify({kind:'admitted',material:[...material],namespace});},
  cloud_cancel(id){calls.push(['cancel',id]);},
  async cloud_connect(){calls.push('connect');return 4;},
  async cloud_reconnect(){calls.push('reconnect');},
  cloud_refresh_media(){calls.push('refresh-media');},
 };
 const enrollment={
  async inspect({signal}){assert.ok(signal instanceof AbortSignal);calls.push('inspect');return '{"challenge":"opaque-u64:18446744073709551615"}';},
  async apply(payload,{operationKey,signal}){assert.ok(signal instanceof AbortSignal);calls.push(['apply',payload,operationKey]);return '{"reply":1}';},
 };
 const lease={material:Uint8Array.of(1),async save(material){calls.push(['save',[...material]]);this.material=material;}};
 const source=createCloudPeerSource({runtime,lease,cloud:{accountId:'acct',machineId:'m-1',enrollment}});
 return{runtime,lease,source,calls,enrollment,state};
}
const names=calls=>calls.map(value=>Array.isArray(value)?value[0]:value);
test('fresh owner: inspect, durable checkpoint ACK, apply with operation key, verified ACK, then owner refresh',async()=>{
 const f=fixture(),prepared=await f.source.prepare();
 assert.deepEqual(names(f.calls),['begin','step','inspect','enroll','save','ack','step','apply','accept','save','ack','step','complete','save']);
 assert.deepEqual(f.calls[0],['begin',[1],'acct','m-1',undefined]);
 assert.deepEqual(f.calls[3],['enroll','{"challenge":"opaque-u64:18446744073709551615"}']);
 assert.deepEqual(f.calls[7],['apply','{"signed":1}','op-1']);
 assert.deepEqual(f.calls[12],['complete',[3]]);
 assert.deepEqual(prepared.namespace,namespace);
 assert.equal(await f.source.connect(prepared,[],()=>{}),4);assert.equal(f.calls.at(-1)[0],'cancel');
});
test('reload of an enrolled owner never repeats inspect or apply',async()=>{
 const f=fixture({phase:'enrolled'});await f.source.prepare();
 assert.deepEqual(names(f.calls),['begin','step','complete','save']);
});
test('restored apply checkpoint resumes with apply only',async()=>{
 const f=fixture({phase:'apply'});await f.source.prepare();
 assert.ok(!f.calls.includes('inspect'));assert.deepEqual(names(f.calls).slice(0,5),['begin','step','apply','accept','save']);
});
test('failed checkpoint save cannot apply and cancels the Rust attempt',async()=>{
 const f=fixture();f.lease.save=async()=>{throw Error('ACK failed');};
 await assert.rejects(f.source.prepare(),/ACK failed/);
 assert.ok(!f.calls.some(v=>Array.isArray(v)&&v[0]==='apply'));assert.ok(!f.calls.some(v=>Array.isArray(v)&&v[0]==='ack'));assert.deepEqual(f.calls.at(-1),['cancel',1]);
});
test('oversized or non-string native JSON is refused before reaching Rust',async()=>{
 const f=fixture();f.enrollment.inspect=async()=>'x'.repeat(20*1024+1);
 await assert.rejects(f.source.prepare(),/native enrollment JSON/);assert.ok(!f.calls.some(v=>Array.isArray(v)&&v[0]==='enroll'));
 const g=fixture();g.enrollment.inspect=async()=>({challenge:1});
 await assert.rejects(g.source.prepare(),/native enrollment JSON/);
});
test('close during inspect prevents signing, saving and apply',async()=>{
 const f=fixture(),gate=deferred();f.enrollment.inspect=()=>gate.promise;
 const pending=f.source.prepare();for(let i=0;i<4;i++)await Promise.resolve();f.source.close();gate.resolve('{"c":1}');
 await assert.rejects(pending,/closed/);assert.ok(!f.calls.some(v=>Array.isArray(v)&&['enroll','save','apply'].includes(v[0])));
});
test('media refresh requires completed enrollment and reuses the retained identity',async()=>{
 const f=fixture({phase:'enrolled'});await f.source.refresh(7);
 assert.deepEqual(f.calls[0],['begin',[1],'acct','m-1',7]);assert.ok(f.calls.includes('refresh-media'));assert.ok(!f.calls.includes('inspect'));
 const g=fixture();await assert.rejects(g.source.refresh(7),/completed enrollment/);assert.ok(!g.calls.includes('inspect'));
});
test('descriptor discovery is bounded, exact and unauthenticated',async()=>{
 const f=fixture({phase:'enrolled'});f.runtime.cloud_complete=async(_id,material)=>JSON.stringify({kind:'admitted',material:[...material],namespace,descriptorUrl:'https://cloud.example/peer/v3/descriptor'});
 const discovered=[];f.runtime.cloud_descriptor=(id,bytes)=>discovered.push([id,[...bytes]]);
 let url,options;const source=createCloudPeerSource({runtime:f.runtime,lease:f.lease,cloud:{accountId:'acct',machineId:'m-1',enrollment:f.enrollment},fetchImpl:async(u,o)=>{url=u;options=o;return new Response(Uint8Array.of(3,4));}});
 await source.prepare();assert.equal(url,'https://cloud.example/peer/v3/descriptor');assert.equal(options.credentials,'omit');assert.equal(options.redirect,'error');assert.deepEqual(discovered,[[1,[3,4]]]);source.close();
 f.runtime.cloud_complete=async(_id,material)=>JSON.stringify({kind:'admitted',material:[...material],namespace,descriptorUrl:'https://cloud.example/other'});
 const bad=createCloudPeerSource({runtime:f.runtime,lease:f.lease,cloud:{accountId:'acct',machineId:'m-1',enrollment:f.enrollment},fetchImpl:async()=>{throw Error('must not fetch');}});
 await assert.rejects(bad.prepare(),/descriptor URL/);
});
test('invalid adapter is refused at construction',()=>{
 assert.throws(()=>createCloudPeerSource({runtime:{},lease:{},cloud:{accountId:'a',machineId:'m',enrollment:{inspect(){}}}}),/Invalid Cloud owner source/);
});
test('late connected owner is closed and never returned after source disposal',async()=>{
 const f=fixture({phase:'enrolled'}),gate=deferred(),prepared=await f.source.prepare();f.runtime.cloud_connect=()=>gate.promise;f.runtime.close_peer=async id=>f.calls.push(['close-peer',id]);
 const pending=f.source.connect(prepared,[],()=>{});f.source.close();gate.resolve(8);await assert.rejects(pending,/closed/);assert.deepEqual(f.calls.at(-1),['close-peer',8]);
});
function resettable(f){f.runtime.cloud_reset_apply=()=>{f.calls.push('reset');f.staged={bytes:[9],phase:'inspect'};return JSON.stringify({kind:'checkpoint',material:[9],namespace:null});};
 const ack=f.runtime.cloud_acknowledge;f.runtime.cloud_acknowledge=(id,material)=>{if(f.staged&&[...material].join()===f.staged.bytes.join()){f.calls.push(['ack',[...material]]);f.state.phase=f.staged.phase;f.staged=null;return;}return ack(id,material);};return f;}
const immediate=()=>Promise.resolve();
test('an apply refused by the Store returns to Inspect with the same keys through a durable checkpoint',async()=>{
 const f=resettable(fixture());let applies=0;f.enrollment.apply=async(payload,{operationKey})=>{f.calls.push(['apply',payload,operationKey]);if(++applies===1)throw Object.assign(Error('refused'),{code:'apply-refused',reason:'expired'});return '{"reply":1}';};
 await f.source.prepare();
 assert.deepEqual(names(f.calls),['begin','step','inspect','enroll','save','ack','step','apply','reset','save','ack','step','inspect','enroll','save','ack','step','apply','accept','save','ack','step','complete','save']);
});
test('a lost first claim surfaces claimed-elsewhere after returning custody to Inspect',async()=>{
 const f=resettable(fixture({phase:'apply'}));f.enrollment.apply=async()=>{throw Object.assign(Error('bound'),{code:'claimed-elsewhere'});};
 await assert.rejects(f.source.prepare(),error=>error.code==='claimed-elsewhere'&&/another owner/.test(error.message));
 assert.equal(f.state.phase,'inspect');assert.equal(f.calls.at(-1)[0],'cancel');
 const g=resettable(fixture({phase:'apply'}));g.enrollment.apply=async()=>{throw Object.assign(Error('future cert'),{code:'apply-refused',reason:'clock-skew'});};
 await assert.rejects(g.source.prepare(),error=>error.code==='clock-skew');assert.equal(g.state.phase,'inspect');
 const h=resettable(fixture({phase:'apply'}));h.enrollment.apply=async()=>{throw Object.assign(Error('no'),{code:'apply-refused'});};h.enrollment.inspect=async()=>'{"c":1}';
 await assert.rejects(h.source.prepare(),error=>error.code==='apply-refused');assert.equal(h.calls.filter(v=>v==='reset').length,2);
});
test('transient adapter failures retry in place with the same operation key',async()=>{
 const f=fixture();let inspects=0,applies=0;
 f.enrollment.inspect=async()=>{if(++inspects<3)throw Object.assign(Error('HTTP 503'),{status:503});return '{"c":1}';};
 f.enrollment.apply=async(_p,{operationKey})=>{f.calls.push(['apply-key',operationKey]);if(++applies<2)throw Object.assign(Error('down'),{code:'unavailable'});return '{"reply":1}';};
 const source=createCloudPeerSource({runtime:f.runtime,lease:f.lease,cloud:{accountId:'acct',machineId:'m-1',enrollment:f.enrollment},delay:immediate});
 await source.prepare();assert.equal(inspects,3);assert.deepEqual(f.calls.filter(v=>v[0]==='apply-key').map(v=>v[1]),['op-1','op-1']);
 const g=fixture();g.enrollment.inspect=async()=>{throw Object.assign(Error('HTTP 503'),{status:503});};
 const busy=createCloudPeerSource({runtime:g.runtime,lease:g.lease,cloud:{accountId:'acct',machineId:'m-1',enrollment:g.enrollment},delay:immediate});
 await assert.rejects(busy.prepare(),error=>error.code==='busy'&&error.cause.status===503);assert.equal(g.calls.at(-1)[0],'cancel');
});
test('runtime refusals reach the caller as typed reason codes with product copy',async()=>{
 for(const [message,code] of [['Cloud owner clock skew: the Cloud challenge expired','clock-skew'],['Cloud owner claimed elsewhere: bound','claimed-elsewhere'],['onboarding HTTP reply rejected (503)','busy'],['onboarding browser operation failed','unavailable'],['Cloud owner membership rejected (403)','membership-refused']]){
  const f=fixture();f.runtime.cloud_enroll=()=>{throw message;};
  await assert.rejects(f.source.prepare(),error=>error.code===code&&error.detail===message&&error.message!==message,message);
 }
 const f=fixture();f.runtime.cloud_enroll=()=>{throw 'something unrelated';};await assert.rejects(f.source.prepare(),error=>error==='something unrelated');
});
test('every failure between prepare and connect releases the prepared Rust attempt',async()=>{
 const f=fixture({phase:'enrolled'});const stale=await f.source.prepare();
 // The caller failed before connect: the next prepare releases the stale slot.
 const fresh=await f.source.prepare();assert.deepEqual(f.calls.filter(v=>Array.isArray(v)&&v[0]==='cancel'),[['cancel',1]]);
 await assert.rejects(f.source.connect(stale,[],()=>{}),/replaced/);
 await f.source.discard(fresh);assert.deepEqual(f.calls.filter(v=>Array.isArray(v)&&v[0]==='cancel').map(v=>v[1]),[1,2]);
 const g=fixture({phase:'enrolled'}),prepared=await g.source.prepare();g.runtime.cloud_reconnect=async()=>{throw Error('reconnect failed');};
 await assert.rejects(g.source.reconnect(prepared,4),/reconnect failed/);assert.equal(g.calls.at(-1)[0],'cancel');
});
test('transport-only resume needs no attempt and no membership refresh',async()=>{
 const f=fixture({phase:'enrolled'});assert.equal(f.source.resume,undefined);
 f.runtime.cloud_resume_transport=async(peer,material,account,machine)=>f.calls.push(['resume',peer,[...material],account,machine]);
 const source=createCloudPeerSource({runtime:f.runtime,lease:f.lease,cloud:{accountId:'acct',machineId:'m-1',enrollment:f.enrollment}});
 await source.resume(4);assert.deepEqual(f.calls,[['resume',4,[1],'acct','m-1']]);
 source.close();await assert.rejects(source.resume(4),/closed/);
});
