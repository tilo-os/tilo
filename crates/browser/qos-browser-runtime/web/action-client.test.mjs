import test from 'node:test';
import assert from 'node:assert/strict';
import {createActionClient} from './action-client.mjs';
const host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', id='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';
const context={room:'cccccccc-cccc-4ccc-8ccc-cccccccccccc',sender:'sender',senderRoot:'root',target:'native',targetRoot:'root',certificate:'exact'};
const route={app_peer:'7',app_id:'writer',record_id:'tab:1',resource:'document',action:'insert',revision:null};
const catalogue={version:1,surfaces:[{peer_id:7,app_id:'writer',record_id:'tab:1',actions:[{resource:'document',action:'insert',contract:{confirmation:{mode:'none'}}}]}]};
function fixture(){let bytes,deny=false;const calls=[];const f={calls,context:{...context},host,get bytes(){return bytes;},set deny(v){deny=v;}};f.open=async()=>createActionClient({context:()=>f.context,load:async()=>bytes,commit:async value=>{if(deny)throw Error('quota');bytes=structuredClone(value);},request:async message=>{calls.push(structuredClone(message));if(message.type==='catalogue_refresh')return{type:'broker_catalogue',host:f.host,catalogue};assert.ok(bytes,'invocation must be durable before transport');return f.response??{type:'observation',host:f.host,invocation:message.invocation,update:null,uncertain:false,cancellation_requested:message.type==='cancel'};}});return f;}
test('commit precedes send; failed commit sends nothing; full UUID and exact intent survive restart',async()=>{const f=fixture(),c=await f.open();f.deny=true;await assert.rejects(c.invoke({...route,invocation:id,argument:{secret:1}}),/quota/);assert.equal(f.calls.filter(m=>m.type==='invoke_bound').length,0);f.deny=false;await c.invoke({...route,invocation:id,argument:{secret:1}});assert.equal(f.calls.find(m=>m.type==='invoke_bound').invocation,id);assert.deepEqual(f.bytes.entries[0].binding,context);const restarted=await f.open();await restarted.status(id);assert.equal(f.calls.filter(m=>m.type==='invoke_bound').length,1);await assert.rejects(restarted.invoke({...route,invocation:id,argument:{secret:2}}),/intent/);});
test('unknown observation is unknown; cancellation acknowledgement is not completion',async()=>{const f=fixture(),c=await f.open();let row=await c.invoke({...route,invocation:id});assert.equal(row.state,'unknown');row=await c.cancel(id);assert.equal(row.state,'unknown');assert.equal(row.cancellationRequested,true);assert.equal(row.terminal,false);});
test('host or sender change refuses recovery to another owner',async()=>{const f=fixture(),c=await f.open();await c.invoke({...route,invocation:id});f.host='dddddddd-dddd-4ddd-8ddd-dddddddddddd';await assert.rejects(c.status(id),/host/);f.host=host;f.context.sender='other';await assert.rejects(c.status(id),/identity/);assert.equal(f.calls.filter(m=>m.type==='status').length,0);});
test('terminal is visible only after observation commit; failed persistence retains uncertainty',async()=>{const f=fixture(),c=await f.open();await c.invoke({...route,invocation:id});f.response={type:'observation',host,invocation:id,update:{state:'succeeded',invocation:id,output:{value:null,resources:[]}},uncertain:false,cancellation_requested:false};f.deny=true;await assert.rejects(c.status(id),/quota/);assert.equal(c.getSnapshot().entries[0].terminal,false);assert.equal(f.calls.filter(message=>message.type==='invoke_bound').length,1);f.deny=false;assert.equal((await c.status(id)).terminal,true);assert.equal(f.calls.filter(message=>message.type==='invoke_bound').length,1,'terminal persistence retry observes status without resubmitting the effect');});
test('only a terminal observation of a durable invocation requests transaction-boundary priority',async()=>{
 const commits=[];let disk;
 const c=await createActionClient({context:()=>context,load:async()=>disk,commit:async(value,_control,options)=>{commits.push(options?.kind??'ordinary');disk=structuredClone(value);},request:async message=>{
  if(message.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue};
  return{type:'observation',host,invocation:id,update:{state:message.type==='invoke_bound'?'accepted':'succeeded',invocation:id},uncertain:false,cancellation_requested:false};
 }});
 assert.equal((await c.invoke({...route,invocation:id})).terminal,false);
 assert.deepEqual(commits,['ordinary','ordinary'],'durable intent and nonterminal response retain the ordinary barrier');
 assert.equal((await c.status(id)).terminal,true);
 assert.deepEqual(commits,['ordinary','ordinary','terminal-observation']);
 await c.close();
});
test('caller cancellation after an authenticated terminal response does not discard its durable observation',async()=>{
 const abort=new AbortController();let disk;
 const c=await createActionClient({context:()=>context,load:async()=>disk,commit:async value=>{disk=structuredClone(value);},request:async message=>{
  if(message.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue};
  if(message.type==='status'){abort.abort();return{type:'observation',host,invocation:id,update:{state:'succeeded',invocation:id},uncertain:false,cancellation_requested:false};}
  return{type:'observation',host,invocation:id,update:{state:'accepted',invocation:id},uncertain:false,cancellation_requested:false};
 }});
 await c.invoke({...route,invocation:id});
 await assert.rejects(c.status(id,{signal:abort.signal}),/cancel|abort/i);
 await drain();assert.equal(disk.entries[0].terminal,true);
 await c.close();
});
test('stale route and undeclared confirmation refuse before admission',async()=>{const f=fixture(),c=await f.open();await assert.rejects(c.invoke({...route,invocation:id,record_id:'tab:2'}),/catalogue/);await assert.rejects(c.invoke({...route,invocation:id,confirmation:{kind:'confirm'}}),/confirmation/);assert.equal(f.bytes,undefined);});
test('private response invocation and host must match',async()=>{const f=fixture(),c=await f.open();f.response={type:'observation',host,invocation:'ffffffff-ffff-4fff-8fff-ffffffffffff',update:null,uncertain:false,cancellation_requested:false};await assert.rejects(c.invoke({...route,invocation:id}),/correlation/);assert.equal(c.getSnapshot().entries[0].state,'unknown');});
test('duplicate delivery cannot regress durable terminal',async()=>{const f=fixture(),c=await f.open();await c.invoke({...route,invocation:id});f.response={type:'observation',host,invocation:id,update:{state:'succeeded',invocation:id,output:{resources:[]}},uncertain:false,cancellation_requested:false};await c.status(id);f.response={type:'observation',host,invocation:id,update:{state:'accepted',invocation:id},uncertain:false,cancellation_requested:false};assert.equal((await c.status(id)).state,'succeeded');});
test('permission refusal and disconnection retain immutable unknown work',async()=>{const f=fixture(),c=await f.open();f.response={type:'rejected',code:'PermissionDenied'};let row=await c.invoke({...route,invocation:id});assert.equal(row.reason,'PermissionDenied');assert.equal(row.terminal,false);assert.equal(f.bytes.entries.length,1);});
test('outbox capacity never evicts unresolved work',async()=>{const f=fixture(),c=await f.open();for(let i=1;i<=256;i++)await c.invoke({...route,invocation:`bbbbbbbb-bbbb-4bbb-8bbb-${String(i).padStart(12,'0')}`});await assert.rejects(c.invoke({...route,invocation:id}),/capacity/);assert.equal(f.bytes.entries.length,256);assert.equal(f.calls.filter(m=>m.type==='invoke_bound').length,256);});
test('bounded concurrent calls reject overload without silent dropping',async()=>{let release;const held=new Promise(resolve=>release=resolve);const c=await createActionClient({context:()=>context,load:async()=>undefined,commit:async()=>{},request:async()=>{await held;return{type:'broker_catalogue',host,catalogue};}});const calls=Array.from({length:32},()=>c.catalogue());await assert.rejects(c.catalogue(),/overload/);release();await Promise.all(calls);});
test('disconnect after durable admission resumes status and never resubmits',async()=>{const f=fixture();let fail=true;const original=f.open;const c=await createActionClient({context:()=>context,load:async()=>undefined,commit:async value=>{f.saved=structuredClone(value);},request:async message=>{f.calls.push(message);if(message.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue};if(fail)throw Error('disconnected');return{type:'observation',host,invocation:id,update:null,uncertain:true,cancellation_requested:false};}});await assert.rejects(c.invoke({...route,invocation:id}),/disconnected/);fail=false;await c.recover();assert.equal(f.calls.filter(m=>m.type==='invoke_bound').length,1);assert.equal(f.calls.filter(m=>m.type==='status').length,1);assert.equal(c.getSnapshot().entries[0].state,'unknown');});
test('confirmation phrase must match the exact broker declaration',async()=>{const confirmed=structuredClone(catalogue);confirmed.surfaces[0].actions[0].contract.confirmation={mode:'phrase',phrase:'DELETE'};const sent=[];const c=await createActionClient({context:()=>context,load:async()=>undefined,commit:async()=>{},request:async m=>{sent.push(m);return m.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue:confirmed}:{type:'observation',host,invocation:id,update:null,uncertain:false,cancellation_requested:false};}});await assert.rejects(c.invoke({...route,invocation:id,confirmation:{kind:'phrase',phrase:'delete'}}),/phrase/);await c.invoke({...route,invocation:id,confirmation:{kind:'phrase',phrase:'DELETE'}});assert.equal(sent.filter(m=>m.type==='invoke_bound').length,1);await c.status(id);assert.equal(sent.filter(m=>m.type==='invoke_bound').length,1);});
test('caller cannot change queued intent by mutating its input',async()=>{const f=fixture(),c=await f.open();const input={...route,invocation:id,argument:{text:'original'}};const task=c.invoke(input);input.argument.text='changed';await task;assert.equal(f.bytes.entries[0].intent.argument.text,'original');});
test('corrupt and newer outboxes fail closed',async()=>{for(const value of [{version:2,entries:[]},{version:1,entries:[{invocation:id}]}])await assert.rejects(createActionClient({context:()=>context,load:async()=>value,commit:async()=>{},request:async()=>{throw Error('must not send');}}),/outbox/);});
test('older accepted/progress cannot regress a durable stream sequence',async()=>{const f=fixture(),c=await f.open();await c.invoke({...route,invocation:id});const observation=update=>({type:'observation',host,invocation:id,update,uncertain:false,cancellation_requested:false});f.response=observation({state:'progress',invocation:id,sequence:'9007199254740993',phase:'working'});await c.status(id);f.response=observation({state:'accepted',invocation:id});assert.equal((await c.status(id)).update.sequence,'9007199254740993');f.response=observation({state:'progress',invocation:id,sequence:'9007199254740992',phase:'older'});assert.equal((await c.status(id)).update.phase,'working');});
test('automatic recovery exposes host mismatch and preserves original destination',async()=>{const f=fixture(),c=await f.open();await c.invoke({...route,invocation:id});f.host='dddddddd-dddd-4ddd-8ddd-dddddddddddd';await c.recover();assert.equal(c.getSnapshot().entries[0].reason,'host-changed');assert.equal(f.bytes.entries[0].host,host);assert.equal(f.calls.filter(m=>m.type==='status').length,0);});

test('a retained palette control cannot rebind to a replacement certified host',async()=>{
 const f=fixture(), c=await f.open();const offered=await c.catalogue();
 f.host='dddddddd-dddd-4ddd-8ddd-dddddddddddd';
 await assert.rejects(c.invoke({...route,invocation:id,expected:{host:offered.host,binding:offered.binding,descriptor:offered.catalogue.surfaces[0].actions[0]}}),/binding|catalogue/i);
 assert.equal(f.bytes,undefined);assert.equal(f.calls.filter(m=>m.type==='invoke_bound').length,0);
});

const deferred=()=>{let resolve;const promise=new Promise(r=>resolve=r);return{promise,resolve};};
const drain=async()=>{for(let i=0;i<30;i++)await Promise.resolve();};
test('empty and terminal restart recovery performs no catalogue work or effect resubmission',async()=>{
 const f=fixture(),c=await f.open();await c.recover();assert.equal(f.calls.length,0);
 await c.invoke({...route,invocation:id});f.response={type:'observation',host,invocation:id,update:{state:'succeeded',invocation:id},uncertain:false};await c.status(id);await c.close();f.calls.length=0;const restarted=await f.open();await restarted.recover();assert.equal(f.calls.length,0);assert.equal(restarted.getSnapshot().entries[0].terminal,true);await restarted.close();
});
test('abort survives the serial queue without durable admission',async()=>{
 const hold=deferred(),sent=[];let saved;
 const c=await createActionClient({context:()=>context,load:async()=>undefined,commit:async v=>saved=v,request:async m=>{sent.push(m);await hold.promise;return{type:'broker_catalogue',host,catalogue};}});
 const first=c.catalogue(),abort=new AbortController(),queued=c.invoke({...route,invocation:id},{signal:abort.signal});
 abort.abort();const rejected=assert.rejects(queued,/cancel|abort/i);await drain();hold.resolve();await first;await rejected;await c.close();assert.equal(saved,undefined);assert.equal(sent.length,1);
});
test('abort during commit reserves admission and keeps serial ordering until commit settles',async()=>{
 const entered=deferred(),hold=deferred(),sent=[];let saved;
 const c=await createActionClient({context:()=>context,load:async()=>undefined,commit:async v=>{entered.resolve();await hold.promise;saved=structuredClone(v);},request:async m=>{sent.push(m);return m.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue}:{type:'observation',host,invocation:id,update:null,uncertain:true};}});
 const abort=new AbortController(),pending=c.invoke({...route,invocation:id},{signal:abort.signal});await entered.promise;
 assert.equal(c.getInvocationStage?.(id),'admitting');abort.abort();await assert.rejects(pending,/cancel|abort/i);
 const following=c.catalogue();await drain();assert.equal(sent.length,1,'following closure must wait for commit');hold.resolve();await following;
 assert.equal(saved.entries[0].invocation,id);assert.equal(sent.filter(m=>m.type==='invoke_bound').length,0);await c.invoke({...route,invocation:id});assert.equal(sent.filter(m=>m.type==='status').length,1);await c.close();
});
test('current route must still be live and in the bound cluster',async()=>{
 for(const change of [{live:false},{cluster_id:'different-room'}]){const changed=structuredClone(catalogue);Object.assign(changed.surfaces[0],change);let saved;
 const c=await createActionClient({context:()=>context,load:async()=>undefined,commit:async v=>saved=v,request:async()=>({type:'broker_catalogue',host,catalogue:changed})});
 await assert.rejects(c.invoke({...route,invocation:id}),/catalogue/);assert.equal(saved,undefined);await c.close();}
});
test('only concrete transient catalogue codes acquire discovery retry metadata',async()=>{
 for(const code of ['Offline','Backpressure','AdmissionUnavailable','NotAuthorised','InvalidRequest','UnsupportedPeer','PermissionDenied']){
 const c=await createActionClient({context:()=>context,load:async()=>undefined,commit:async()=>{},request:async()=>({type:'rejected',code})});
 await assert.rejects(c.catalogue(),error=>error.stage==='discovery'&&error.retryable===['Offline','Backpressure','AdmissionUnavailable'].includes(code));await c.close();}
});
test('identity revocation after commit retains the row and prevents effect send',async()=>{
 let revoked=false,saved;const sent=[];const c=await createActionClient({context:()=>{if(revoked)throw Error('identity revoked');return context;},load:async()=>undefined,commit:async v=>{saved=v;revoked=true;},request:async m=>{sent.push(m);return{type:'broker_catalogue',host,catalogue};}});
 await assert.rejects(c.invoke({...route,invocation:id}),/revoked/);assert.equal(saved.entries[0].invocation,id);assert.equal(sent.filter(m=>m.type==='invoke_bound').length,0);await c.close();
});
test('queued deadline settles the caller while retaining the serial operation guard',async()=>{
 let time=0;const callbacks=new Set(),clock={now:()=>time,setTimeout:fn=>{callbacks.add(fn);return fn;},clearTimeout:fn=>callbacks.delete(fn)};const hold=deferred(),sent=[];let saved;
 const c=await createActionClient({clock,context:()=>context,load:async()=>undefined,commit:async v=>saved=v,request:async m=>{sent.push(m);await hold.promise;return{type:'broker_catalogue',host,catalogue};}});const first=c.catalogue(),queued=c.invoke({...route,invocation:id},{deadlineAtMs:100});await drain();time=100;for(const fn of [...callbacks])fn();await assert.rejects(queued,/not sent/);hold.resolve();await first;await c.close();assert.equal(saved,undefined);assert.equal(sent.length,1);
});


test('fresh catalogue selection and invocation share one serial turn and one discovery',async()=>{
 const f=fixture(),c=await f.open();
 await c.invokeSelected(current=>{assert.equal(current.host,host);return {...route,invocation:id};});
 assert.deepEqual(f.calls.map(m=>m.type),['catalogue_refresh','invoke_bound']);
 assert.equal(f.bytes.entries[0].invocation,id);
 await c.close();
});
test('exact route refresh does not replace the public palette catalogue',async()=>{
 const publicCatalogue=structuredClone(catalogue);publicCatalogue.surfaces.push({peer_id:8,app_id:'notes',record_id:'tab:2',actions:[{resource:'page',action:'open'}]});
 let saved;const sent=[];
 const c=await createActionClient({context:()=>context,load:async()=>saved,commit:async value=>saved=structuredClone(value),request:async m=>{
  sent.push(structuredClone(m));
  if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:m.scope?catalogue:publicCatalogue};
  return{type:'observation',host,invocation:m.invocation,update:null,uncertain:true};
 }});
 await c.catalogue();await c.invoke({...route,invocation:id});
 assert.deepEqual(c.getSnapshot().capability.catalogue,publicCatalogue);
 assert.deepEqual(sent[1].scope,{app_id:'writer',record_id:'tab:1',resource:'document',action:'insert'});
 await c.close();
});
test('detached palette discovery does not occupy the action admission lane',async()=>{
 const full=deferred(),entered=deferred(),sent=[];let saved;
 const c=await createActionClient({context:()=>context,load:async()=>saved,commit:async value=>saved=structuredClone(value),request:async m=>{
  sent.push(structuredClone(m));
  if(m.type==='catalogue_refresh'&&!m.scope){entered.resolve();await full.promise;return{type:'broker_catalogue',host,catalogue};}
  if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue};
  return{type:'observation',host,invocation:m.invocation,update:null,uncertain:true};
 }});
 const palette=c.refreshCatalogue();await entered.promise;
 const row=await c.invoke({...route,invocation:id});
 assert.equal(row.invocation,id);assert.deepEqual(sent.slice(0,3).map(m=>[m.type,!!m.scope]),[['catalogue_refresh',false],['catalogue_refresh',true],['invoke_bound',false]]);
 full.resolve();await palette;await c.close();
});
test('an older palette discovery cannot replace a newer identity capability',async()=>{
 const old=deferred(),entered=deferred();let binding={...context},requests=0;
 const replacement={...context,target:'replacement'};
 const newer=structuredClone(catalogue);newer.surfaces[0].record_id='replacement';
 const c=await createActionClient({context:()=>binding,load:async()=>undefined,commit:async()=>{},request:async m=>{
  assert.equal(m.type,'catalogue_refresh');if(++requests===1){entered.resolve();await old.promise;return{type:'broker_catalogue',host,catalogue};}
  return{type:'broker_catalogue',host,catalogue:newer};
 }});
 const stale=c.refreshCatalogue();await entered.promise;binding=replacement;await c.refreshCatalogue();
 old.resolve();await assert.rejects(stale,/identity/i);
 assert.deepEqual(c.getSnapshot().capability.binding,replacement);assert.deepEqual(c.getSnapshot().capability.catalogue,newer);await c.close();
});
test('known route recovery remains scoped when the full catalogue is oversized',async()=>{
 let saved,overflow=false;const sent=[];
 const c=await createActionClient({context:()=>context,load:async()=>saved,commit:async value=>saved=structuredClone(value),request:async m=>{
  sent.push(structuredClone(m));
  if(m.type==='catalogue_refresh')return overflow&&!m.scope?{type:'rejected',code:'action_response_exceeds_limits'}:{type:'broker_catalogue',host,catalogue};
  return{type:'observation',host,invocation:m.invocation,update:null,uncertain:true};
 }});
 await c.invoke({...route,invocation:id});overflow=true;sent.length=0;await c.recover();
 assert.equal(sent.at(-1).type,'status');
 assert(sent.filter(m=>m.type==='catalogue_refresh').every(m=>m.scope?.app_id==='writer'));
 await c.close();
});
test('selected invocation checks binding again after selection and after durable admission',async()=>{
 const f=fixture(),c=await f.open();
 await assert.rejects(c.invokeSelected(()=>{f.context={...context,certificate:'replacement'};return {...route,invocation:id};}),/identity/i);
 assert.equal(f.bytes,undefined);assert.equal(f.calls.filter(m=>m.type==='invoke_bound').length,0);
 await c.close();
});
test('foreground work runs between retained recovery observations and concurrent recovery is coalesced',async()=>{
 const gate=deferred(),entered=deferred(),sent=[];let disk,hold=false;
 const ids=[id,'eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee'];
 const c=await createActionClient({context:()=>context,load:async()=>disk,commit:async value=>{disk=value;},request:async m=>{
  sent.push(m.type+(m.invocation?':'+m.invocation:''));
  if(m.type==='catalogue_refresh')return {type:'broker_catalogue',host,catalogue};
  if(hold&&m.type==='status'&&m.invocation===id){entered.resolve();await gate.promise;}
  return {type:'observation',host,invocation:m.invocation,update:null,uncertain:true};
 }});
 for(const invocation of ids)await c.invoke({...route,invocation});
 sent.length=0;hold=true;const recovery=c.recover();await entered.promise;
 const duplicate=c.recover(),foreground=c.catalogue();gate.resolve();
 await Promise.all([recovery,duplicate,foreground]);
 assert.equal(sent.filter(type=>type==='status:'+id).length,1);
 assert.equal(sent.filter(type=>type==='status:'+ids[1]).length,1);
 const first=sent.indexOf('status:'+id),second=sent.indexOf('status:'+ids[1]);
 assert.equal(sent[first+1],'catalogue_refresh','foreground discovery must not wait for the second retained status');
 assert(second>first+1);
 await c.close();
});

test('a background status round trip cannot block a new invocation or overwrite its durable row',async t=>{
 const gate=deferred(),entered=deferred();let disk,hold=false;
 const fresh='ffffffff-ffff-4fff-8fff-ffffffffffff';
 const c=await createActionClient({context:()=>context,load:async()=>disk,commit:async value=>{disk=structuredClone(value);},request:async m=>{
  if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue};
  if(hold&&m.type==='status'&&m.invocation===id){entered.resolve();await gate.promise;}
  return{type:'observation',host,invocation:m.invocation,update:m.invocation===fresh||hold?{state:'succeeded',invocation:m.invocation}:null,uncertain:false};
 }});
 t.after(async()=>{gate.resolve();await c.close();});
 await c.invoke({...route,invocation:id});hold=true;
 const recovery=c.recover();await entered.promise;
 let opened=false;const opening=c.invoke({...route,invocation:fresh}).then(()=>{opened=true;});
 for(let i=0;i<100;i++)await Promise.resolve();
 assert.equal(opened,true,'foreground command must not wait for an unrelated status response');
 gate.resolve();await Promise.all([recovery,opening]);
 assert.deepEqual(disk.entries.map(row=>[row.invocation,row.terminal]),[[id,true],[fresh,true]]);
});

test('late background observations cannot replace a newer terminal result',async t=>{
 const gate=deferred(),entered=deferred();let disk,statuses=0;
 const c=await createActionClient({context:()=>context,load:async()=>disk,commit:async value=>{disk=structuredClone(value);},request:async m=>{
  if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue};
  if(m.type==='invoke_bound')return{type:'observation',host,invocation:id,update:null,uncertain:false};
  const first=++statuses===1;if(first){entered.resolve();await gate.promise;}
  return{type:'observation',host,invocation:id,update:{state:first?'running':'succeeded',sequence:first?'1':'2',invocation:id},uncertain:false};
 }});
 t.after(async()=>{gate.resolve();await c.close();});
 await c.invoke({...route,invocation:id});const recovery=c.recover();await entered.promise;
 await c.status(id);gate.resolve();await recovery;
 assert.equal(disk.entries[0].state,'succeeded');assert.equal(disk.entries[0].update.sequence,'2');
});
test('closing during background network work prevents late persistence',async()=>{
 const gate=deferred(),entered=deferred();let disk,writes=0;
 const c=await createActionClient({context:()=>context,load:async()=>disk,commit:async value=>{writes++;disk=value;},request:async m=>{
  if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue};
  if(m.type==='status'){entered.resolve();await gate.promise;}
  return{type:'observation',host,invocation:id,update:null,uncertain:false};
 }});
 await c.invoke({...route,invocation:id});const recovery=c.recover();await entered.promise;
 let closed=false;const closing=c.close().then(()=>closed=true);const before=writes;await drain();assert.equal(closed,false);gate.resolve();await closing;await recovery;assert.equal(writes,before);
});

test('background discovery cannot send status after a subscriber replaces the identity',async()=>{
 let binding={...context},disk,replace=false;const sent=[];
 const c=await createActionClient({context:()=>binding,load:async()=>disk,commit:async value=>{disk=value;},request:async m=>{
  sent.push(m.type);if(m.type==='catalogue_refresh'){if(replace&&m.scope)binding={...context,target:'replacement'};return{type:'broker_catalogue',host,catalogue};}return{type:'observation',host,invocation:id,update:null,uncertain:false};
 }});
 await c.invoke({...route,invocation:id});sent.length=0;replace=true;
 await c.recover().catch(()=>{});
 assert.deepEqual(sent,['catalogue_refresh']);await c.close();
});

test('live retained drive resends original after correlated absence, never after admission or reopen',async()=>{
 const sent=[];let disk, admitted=false,drop=true;
 const cat=structuredClone(catalogue);cat.surfaces[0].actions[0].revision='exact-v1';
 const open=()=>createActionClient({context:()=>context,load:async()=>disk,commit:async v=>disk=structuredClone(v),request:async m=>{
  sent.push(structuredClone(m));if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:cat};
  if(m.type==='invoke_bound'){if(drop){drop=false;throw Error('before receiver');}admitted=true;}
  return{type:'observation',host,invocation:id,update:admitted?{state:'accepted',invocation:id}:null,uncertain:false,cancellation_requested:false};
 }});
 const c=await open();await assert.rejects(c.invoke({...route,revision:'exact-v1',invocation:id,argument:{original:1}}),/before receiver/);
 const original=structuredClone(disk.entries[0].intent);
 assert.equal(typeof c.driveRetained,'function');
 await Promise.all([c.driveRetained(id),c.driveRetained(id)]);
 assert.deepEqual(sent.filter(m=>m.type==='invoke_bound'),[original,original]);
 await c.driveRetained(id);assert.equal(sent.filter(m=>m.type==='invoke_bound').length,2);
 await c.close();admitted=false;const reopened=await open();await reopened.driveRetained(id);
 assert.equal(sent.filter(m=>m.type==='invoke_bound').length,2);await reopened.close();
});

test('ambiguous persistence latches all RPCs until the attempted write reconciles',async()=>{
 let saved,blocked=true,first=true;const calls=[];
 const c=await createActionClient({context:()=>context,load:async()=>undefined,commit:async v=>{saved=structuredClone(v);if(first){first=false;throw Object.assign(Error('lost acknowledgement'),{name:'StorageWorkerError'});}},reconcile:async()=>{if(blocked)throw Error('unresolved storage');return 'committed';},request:async m=>{calls.push(m);return m.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue}:{type:'observation',host,invocation:id,update:null,uncertain:false,cancellation_requested:false};}});
 await assert.rejects(c.invoke({...route,invocation:id}),/lost acknowledgement/);const before=calls.length;
 await assert.rejects(c.catalogue(),/storage/i);assert.equal(calls.length,before);
 blocked=false;await c.catalogue();assert.deepEqual(c.getSnapshot().entries,saved.entries);await c.close();
});

test('close joins raw detached catalogue and recovery requests',async()=>{
 const hold=deferred(),entered=deferred();let closed=false;
 const c=await createActionClient({context:()=>context,load:async()=>undefined,commit:async()=>{},request:async()=>{entered.resolve();await hold.promise;return{type:'broker_catalogue',host,catalogue};}});
 const pending=c.refreshCatalogue();await entered.promise;const rejected=assert.rejects(pending,/cancel|abort/i);
 const closing=c.close().then(()=>closed=true);await drain();assert.equal(closed,false);
 hold.resolve();await rejected;await closing;
});

test('business cancel synchronously fences an already-running retained drive',async()=>{
 const gate=deferred(),entered=deferred();let phase='first',disk;const sent=[];
 const cat=structuredClone(catalogue);cat.surfaces[0].actions[0].revision='v1';
 const c=await createActionClient({context:()=>context,load:async()=>undefined,commit:async v=>disk=v,request:async m=>{
  sent.push(structuredClone(m));if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:cat};
  if(m.type==='invoke_bound')throw Error('not delivered');
  if(phase==='drive'&&m.type==='status'){entered.resolve();await gate.promise;}
  if(m.type==='cancel')throw Error('cancel reply lost');
  return{type:'observation',host,invocation:id,update:null,uncertain:false,cancellation_requested:false};
 }});
 await assert.rejects(c.invoke({...route,revision:'v1',invocation:id}),/not delivered/);phase='drive';
 const driving=c.driveRetained(id);await entered.promise;const cancel=assert.rejects(c.cancel(id),/cancel reply lost/);gate.resolve();await driving;await cancel;
 phase='after';await c.driveRetained(id);assert.equal(sent.filter(m=>m.type==='invoke_bound').length,1);assert.equal(disk.entries.length,1);await c.close();
});

test('retained drive refuses changed route, missing rows, malformed absence and confirmation',async()=>{
 for(const kind of ['route','rejected','uncertain','missing-flag','confirmation','host','identity']){
  let changed=false;const sent=[];const cat=structuredClone(catalogue);cat.surfaces[0].actions[0].revision='v1';
  const c=await createActionClient({context:()=>changed&&kind==='identity'?{...context,certificate:'other'}:context,load:async()=>undefined,commit:async()=>{},request:async m=>{
   sent.push(m);if(m.type==='catalogue_refresh'){const current=structuredClone(cat);if(changed&&kind==='route')current.surfaces[0].actions[0].revision='v2';if(changed&&kind==='confirmation')current.surfaces[0].actions[0].contract.confirmation={mode:'confirm'};return{type:'broker_catalogue',host:changed&&kind==='host'?id:host,catalogue:current};}
   if(m.type==='invoke_bound')throw Error('lost');
   if(kind==='rejected')return{type:'rejected',code:'unknown_invocation'};
   const answer={type:'observation',host,invocation:id,update:null,uncertain:kind==='uncertain',cancellation_requested:false};if(kind==='missing-flag')delete answer.cancellation_requested;return answer;
  }});
  await assert.rejects(c.invoke({...route,revision:'v1',invocation:id}),/lost/);changed=true;
  await c.driveRetained(id).catch(()=>{});const before=sent.length;await assert.rejects(c.driveRetained('ffffffff-ffff-4fff-8fff-ffffffffffff'),/Unknown local/);
  assert.equal(sent.length,before);assert.equal(sent.filter(m=>m.type==='invoke_bound').length,1,kind);await c.close();
 }
});

test('detached catalogue reply cannot restore ready while storage is ambiguous',async()=>{
 const entered=deferred(),held=deferred();let palette=false;
 const c=await createActionClient({context:()=>context,load:async()=>undefined,commit:async()=>{throw Object.assign(Error('lost acknowledgement'),{name:'StorageWorkerError'});},request:async m=>{
  if(!m.scope&&palette){entered.resolve();await held.promise;}
  return{type:'broker_catalogue',host,catalogue};
 }});
 palette=true;const refresh=c.refreshCatalogue();await entered.promise;
 await assert.rejects(c.invoke({...route,invocation:id}),/lost acknowledgement/);held.resolve();
 await assert.rejects(refresh,/reconciliation/);assert.equal(c.getSnapshot().capability.state,'unavailable');await c.close();
});

test('cancel fences the live attempted row before ambiguous admission is reconciled',async()=>{
 let first=true;const sent=[];const cat=structuredClone(catalogue);cat.surfaces[0].actions[0].revision='v1';
 const c=await createActionClient({context:()=>context,load:async()=>undefined,commit:async()=>{if(first){first=false;throw Object.assign(Error('lost acknowledgement'),{name:'StorageWorkerError'});}},reconcile:async()=> 'committed',request:async m=>{
  sent.push(m);if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:cat};if(m.type==='cancel')throw Error('cancel reply lost');return{type:'observation',host,invocation:id,update:null,uncertain:false,cancellation_requested:false};
 }});
 await assert.rejects(c.invoke({...route,revision:'v1',invocation:id}),/lost acknowledgement/);
 await assert.rejects(c.cancel(id),/cancel reply lost/);await c.driveRetained(id);
 assert.equal(sent.filter(m=>m.type==='invoke_bound').length,0);await c.close();
});

test('business cancel fences an accepted invoke while queued or discovering without unknown-ID tombstones',async()=>{
 for(const phase of ['queued','discovering']){
  const gate=deferred(),entered=deferred();let held=true,writes=0;const sent=[];
  const c=await createActionClient({context:()=>context,load:async()=>undefined,commit:async()=>{writes++;},request:async m=>{
   sent.push(m);if(held&&m.type==='catalogue_refresh'){entered.resolve();await gate.promise;}
   return m.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue}:{type:'observation',host,invocation:m.invocation,update:null,uncertain:false,cancellation_requested:false};
  }});
  let first;if(phase==='queued'){first=c.catalogue();await entered.promise;}
  const invoke=c.invoke({...route,invocation:id});if(phase==='discovering')await entered.promise;
  const rejected=assert.rejects(invoke,/cancel/i);const cancel=c.cancel(id).catch(e=>e);held=false;gate.resolve();await first;await rejected;await cancel;
  assert.equal(sent.filter(m=>m.type==='invoke_bound').length,0,phase);assert.equal(writes,0,phase);
  // An unknown ID must not leave a tombstone that blocks a future deliberate command.
  await c.cancel('ffffffff-ffff-4fff-8fff-ffffffffffff').catch(()=>{});
  await c.invoke({...route,invocation:'ffffffff-ffff-4fff-8fff-ffffffffffff'});assert.equal(sent.filter(m=>m.type==='invoke_bound').length,1);await c.close();
 }
});
test('a fresh v2 ticket dispatches at once; a stale or reopened one probes status against a new discovery first',async()=>{
 let t=0;const consumer=parent=>({kind:'interactive',parent,slot:'test'});
 for(const [delay,expected] of [[0,['catalogue_refresh','invoke_bound']],[6000,['catalogue_refresh','catalogue_refresh','status','invoke_bound']]]){
  const calls=[];let disk;
  const c=await createActionClient({version:2,clock:{now:()=>t},context:()=>context,load:async()=>disk,commit:async value=>{disk=structuredClone(value);},request:async message=>{
   calls.push(message.type);
   if(message.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue};
   return{type:'observation',host,invocation:message.invocation,update:message.type==='invoke_bound'?{state:'succeeded',invocation:message.invocation,output:{}}:null,uncertain:false,cancellation_requested:false};
  }});
  const {invocation:_none,...input}={...route,argument:null,confirmation:null};
  const ticket=await c.actionsV2.beginNew(input,{consumer:consumer(crypto.randomUUID())});
  t+=delay;const row=await c.actionsV2.drive(ticket,{dispatchFresh:true});
  assert.equal(row.state,'succeeded');assert.deepEqual(calls,expected);
  await c.close();
 }
});
test('background recovery backs off an unchanged row to 16 s, reuses its discovery for 30 s, and resets on change',async()=>{
 let t=0,disk,update=null;const calls=[];const second='dddddddd-dddd-4ddd-8ddd-dddddddddddd';
 const c=await createActionClient({clock:{now:()=>t},context:()=>context,load:async()=>disk,commit:async value=>{disk=structuredClone(value);},request:async message=>{
  calls.push(message.type);
  if(message.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue};
  return{type:'observation',host,invocation:message.invocation,update:message.type==='status'&&update?{...update,invocation:message.invocation}:{state:'accepted',invocation:message.invocation},uncertain:false,cancellation_requested:false};
 }});
 await c.invoke({...route,invocation:id});await c.invoke({...route,invocation:second});
 const pass=async ms=>{t+=ms;calls.length=0;await c.recover({background:true});return [...calls];};
 const both=['status','status'];
 // Two retained rows of one scope: one discovery, two status probes.
 assert.deepEqual(await pass(0),['catalogue_refresh',...both]);
 assert.deepEqual(await pass(1000),both,'unchanged: next probe after 1 s, discovery reused');
 assert.deepEqual(await pass(1000),[],'backed off to 2 s');
 assert.deepEqual(await pass(1000),both);
 assert.deepEqual(await pass(4000),both);
 assert.deepEqual(await pass(8000),both);
 assert.deepEqual(await pass(16000),['catalogue_refresh',...both],'discovery renewed after 30 s');
 assert.deepEqual(await pass(8000),[],'capped at 16 s');
 assert.deepEqual(await pass(8000),both);
 update={state:'progress',sequence:'1',phase:'working'};
 assert.deepEqual(await pass(16000),['catalogue_refresh',...both],'32 s after the last discovery');
 assert.deepEqual(await pass(1000),both,'a changed observation resets to 1 s');
 // A continuous caller cannot turn this into a status loop.
 calls.length=0;for(let i=0;i<50;i++)await c.recover({background:true});assert.equal(calls.length,0);
 t+=1000;calls.length=0;await c.recover();assert.deepEqual(calls,['catalogue_refresh','status','status'],'an explicit recover probes at once');
 await c.close();
});
