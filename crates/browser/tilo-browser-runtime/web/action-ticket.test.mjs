import test from 'node:test';
import assert from 'node:assert/strict';
import {createActionClient} from './action-client.mjs';
const host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
const binding={room:'room',sender:'sender',senderRoot:'person',target:'host',targetRoot:'root',certificate:'exact'};
const route={app_peer:'7',app_id:'writer',record_id:'record',resource:'document',action:'insert',revision:'original',argument:{text:'original'}};
const catalogue={version:1,surfaces:[{peer_id:'7',cluster_id:'room',app_id:'writer',record_id:'record',actions:[{resource:'document',action:'insert',revision:'original',contract:{confirmation:{mode:'none'}}}]}]};
const consumer=()=>({kind:'interactive',parent:crypto.randomUUID(),slot:'interactive-result'});
function fixture(){
 let disk;const f={calls:[],binding:structuredClone(binding),commits:[],get disk(){return structuredClone(disk);},set disk(v){disk=structuredClone(v);}};
 f.open=async(version=2)=>createActionClient({version,context:()=>f.binding,load:async()=>disk,readLegacyImage:()=>f.legacyImage??JSON.stringify(disk),commit:async value=>{await f.beforeCommit?.(value);disk=structuredClone(value);f.commits.push(structuredClone(value));},request:async message=>{f.calls.push(structuredClone(message));if(message.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue};assert.ok(disk.entries.some(row=>row.invocation===message.invocation),'mutation must be committed');return f.response?.(message)??{type:'observation',host,invocation:message.invocation,update:null,uncertain:false,cancellation_requested:false};}});
 return f;
}
const succeeded=m=>({type:'observation',host,invocation:m.invocation,update:{state:'succeeded',invocation:m.invocation,output:{value:{done:true},resources:[]}},uncertain:false,cancellation_requested:false});
const ack=row=>({consumer:row.ticket.consumer,update:row.update,continuation:{kind:'interactive-result-presented'}});
test('v2 begin persists internal identity and mapping without effect send; reload drives exact original bytes',async()=>{
 const f=fixture(),c=await f.open();assert.ok(c.actionsV2,'separate public ticket capability required');const a=c.actionsV2;
 const owner=consumer(),ticket=await a.beginNew(route,{consumer:owner});assert.equal(f.calls.filter(m=>m.type==='invoke_bound').length,0);assert.deepEqual(f.disk.entries[0].ticket,ticket);
 await assert.rejects(a.beginNew({...route,invocation:crypto.randomUUID()},{consumer:consumer()}),/invocation|fresh/i);await assert.rejects(a.beginNew(route,{consumer:owner}),/slot|consumer/i);
 const original=f.disk.entries[0].intent;await c.close();const reopened=await f.open();assert.deepEqual(reopened.actionsV2.getSnapshot().entries[0].ticket,ticket);await reopened.actionsV2.drive(ticket);assert.deepEqual(f.calls.filter(m=>m.type==='invoke_bound'),[original]);await reopened.close();
});
test('v2 consumes only after durable intended consumer acknowledgement; stale tickets cause zero RPC',async()=>{
 const f=fixture(),c=await f.open();assert.ok(c.actionsV2);const a=c.actionsV2,ticket=await a.beginNew(route,{consumer:consumer()});f.response=succeeded;const row=await a.observe(ticket);
 await assert.rejects(a.consumeTerminal(ticket),/acknowledg/i);await assert.rejects(a.acknowledgeTerminal(ticket,{...ack(row),consumer:consumer()}),/consumer/i);
 await a.acknowledgeTerminal(ticket,ack(row));assert.deepEqual(f.disk.entries[0].consumerAcknowledgement.update,row.update);await a.consumeTerminal(ticket);assert.equal(f.disk.entries.length,0);
 const before=f.calls.length;for(const bad of [ticket,{...ticket,namespace:crypto.randomUUID()},{...ticket,operation:crypto.randomUUID()}])for(const name of ['drive','observe','requestCancellation','consumeTerminal'])await assert.rejects(a[name](bad));assert.equal(f.calls.length,before);await c.close();
});
test('v2 cancellation commits before RPC and survives reload; cancelled or uncertain terminal cannot consume',async()=>{
 const f=fixture(),c=await f.open();assert.ok(c.actionsV2);const a=c.actionsV2,ticket=await a.beginNew(route,{consumer:consumer()});f.response=m=>{if(m.type==='cancel'){assert.equal(f.disk.entries[0].businessCancellationRequested,true);throw Error('lost cancel response');}return succeeded(m);};
 await assert.rejects(a.requestCancellation(ticket),/lost/);await c.close();const reopened=await f.open(),b=reopened.actionsV2;await b.drive(ticket);assert.equal(f.calls.filter(m=>m.type==='invoke_bound').length,0);await assert.rejects(b.acknowledgeTerminal(ticket,ack(b.getSnapshot().entries[0])),/certain|cancel/i);await assert.rejects(b.consumeTerminal(ticket));await reopened.close();
 for(const response of [m=>({...succeeded(m),uncertain:true}),m=>({...succeeded(m),update:{state:'cancelled',invocation:m.invocation,message:null}}),m=>({...succeeded(m),update:{state:'succeeded',invocation:m.invocation}})]){
  const g=fixture(),client=await g.open(),api=client.actionsV2,t=await api.beginNew(route,{consumer:consumer()});g.response=response;await api.observe(t).catch(()=>{});await assert.rejects(api.acknowledgeTerminal(t,ack(api.getSnapshot().entries[0])));await assert.rejects(api.consumeTerminal(t));await client.close();
 }
});
test('promotion preserves exact legacy image, refuses old writer and raw new-ID creation, retains legacy terminal rows',async()=>{
 const f=fixture(),old=await f.open(1);await old.invoke({...route,invocation:'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb'});f.response=succeeded;await old.status('bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb');await old.close();const legacy=f.disk;f.legacyImage=JSON.stringify(legacy,null,2);const client=await f.open();assert.ok(client.actionsV2);assert.equal(f.disk.legacyImage,f.legacyImage);assert.deepEqual(f.disk.entries,legacy.entries);
 await assert.rejects(f.open(1),/outbox|version/i);await assert.rejects(client.invoke({...route,invocation:crypto.randomUUID()}),/legacy|version|fresh/i);assert.equal((await client.status(legacy.entries[0].invocation)).terminal,true);await assert.rejects(client.actionsV2.consumeTerminal({version:2,namespace:f.disk.namespace,operation:legacy.entries[0].invocation,binding,consumer:consumer()}));assert.equal(f.disk.entries.length,1);await client.close();
});
test('more than 256 certain acknowledged and consumed operations retain bounded live rows and bytes',async()=>{
 const f=fixture(),c=await f.open();assert.ok(c.actionsV2);f.response=succeeded;const a=c.actionsV2;let peak=0;
 for(let i=0;i<270;i++){const ticket=await a.beginNew(route,{consumer:consumer()}),row=await a.observe(ticket);await a.acknowledgeTerminal(ticket,ack(row));peak=Math.max(peak,new TextEncoder().encode(JSON.stringify(f.disk)).length);await a.consumeTerminal(ticket);assert.equal(f.disk.entries.length,0);}
 assert.ok(peak<4*1024*1024);assert.equal(f.calls.filter(m=>m.type==='invoke_bound').length,0);await c.close();
});
test('uncertain terminal followed by a nonterminal observation never qualifies for consumption',async()=>{
 const f=fixture(),c=await f.open(),a=c.actionsV2,ticket=await a.beginNew(route,{consumer:consumer()});f.response=m=>({...succeeded(m),uncertain:true});await a.observe(ticket);f.response=m=>({...succeeded(m),update:{state:'accepted',invocation:m.invocation}});const row=await a.observe(ticket);assert.equal(row.terminal,false);assert.equal(row.state,'unknown');assert.equal(row.reason,'terminal-unconfirmed');await assert.rejects(a.acknowledgeTerminal(ticket,ack(row)));await c.close();
});
test('same-row background observation is joined before removal; it cannot resurrect consumed row',async()=>{
 const f=fixture(),c=await f.open(),a=c.actionsV2,ticket=await a.beginNew(route,{consumer:consumer()});let release,entered;const start=new Promise(r=>entered=r);f.response=async m=>{entered();await new Promise(r=>release=r);return succeeded(m);};const recovery=c.recover();await start;
 f.response=succeeded;const row=await a.observe(ticket);await a.acknowledgeTerminal(ticket,ack(row));let removed=false;const consuming=a.consumeTerminal(ticket).then(()=>removed=true);await Promise.resolve();assert.equal(removed,false);release();await recovery;await consuming;assert.equal(f.disk.entries.length,0);await c.recover();assert.equal(f.disk.entries.length,0);await c.close();
});
test('promotion exceeding aggregate image bytes refuses without overwriting the original v1 row',async()=>{
 const f=fixture(),old=await f.open(1);await old.invoke({...route,argument:{text:'a'.repeat(2200000)}});await old.close();const before=f.disk;await assert.rejects(f.open(),/capacity/);assert.deepEqual(f.disk,before);
});
test('synchronous business cancellation fences a queued consumer before durable cancellation settles',async()=>{
 const f=fixture(),c=await f.open(),a=c.actionsV2,ticket=await a.beginNew(route,{consumer:consumer()});f.response=succeeded;const row=await a.observe(ticket);await a.acknowledgeTerminal(ticket,ack(row));
 const consumed=a.consumeTerminal(ticket);await Promise.resolve();const cancelled=a.requestCancellation(ticket);await assert.rejects(consumed,/certain|cancel|acknowledg/i);await cancelled;assert.equal(f.disk.entries.length,1);assert.equal(f.disk.entries[0].businessCancellationRequested,true);await c.close();
});
test('legacy raw-ID drive cannot join an already in-flight v2 ticket drive',async()=>{
 const f=fixture(),c=await f.open(),a=c.actionsV2,ticket=await a.beginNew(route,{consumer:consumer()});
 let entered,release;const started=new Promise(r=>entered=r),held=new Promise(r=>release=r);
 f.response=async m=>{if(m.type==='status'){entered();await held;return{type:'observation',host,invocation:m.invocation,update:null,uncertain:false,cancellation_requested:false};}return succeeded(m);};
 const authorized=a.drive(ticket);await started;const before=f.calls.length;
 const raw=c.driveRetained(ticket.operation).then(()=>({status:'resolved'}),error=>({status:'rejected',message:error.message}));
 try{release();await authorized;const result=await raw;assert.equal(result.status,'rejected');assert.match(result.message,/scoped ticket/);assert.equal(f.calls.length,before+1);assert.equal(f.calls.filter(m=>m.type==='invoke_bound').length,1);}
 finally{release();await authorized.catch(()=>{});await c.close();}
});
test('promotion preserves exact persisted empty v1 image instead of treating it as absent storage',async()=>{
 const f=fixture();f.disk={version:1,entries:[]};f.legacyImage=' { "entries" : [], "version" : 1 }\n';const c=await f.open();assert.equal(f.disk.legacyImage,f.legacyImage);assert.deepEqual(f.disk.entries,[]);await c.close();
 const absent=fixture(),fresh=await absent.open();assert.equal(absent.disk.legacyImage,null);await fresh.close();
});
test('unpromoted v1 still coalesces drives queued behind the same unsettled admission',async()=>{
 const f=fixture(),c=await f.open(1),id='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';let entered,release,held=false;const started=new Promise(r=>entered=r),gate=new Promise(r=>release=r);
 f.beforeCommit=async()=>{if(!held){held=true;entered();await gate;}};const admission=c.invoke({...route,invocation:id});await started;const first=c.driveRetained(id),second=c.driveRetained(id),same=first===second;const results=Promise.allSettled([admission,first,second]);release();await results;assert.equal(same,true);await c.close();
});
test('public legacy drive ignores extra arguments and cannot enter the internal ticket path',async()=>{
 const f=fixture(),c=await f.open(),a=c.actionsV2,ticket=await a.beginNew(route,{consumer:consumer()}),before=f.calls.length;
 await assert.rejects(c.driveRetained(ticket.operation,{},ticket),/scoped ticket/);assert.equal(f.calls.length,before);await c.close();
});

test('v2 live owner dispatches a revision-less exact descriptor once; reopened revision-less ticket fails closed',async()=>{
 const plain={...route,revision:null},cat={version:1,surfaces:[{...catalogue.surfaces[0],actions:[{...catalogue.surfaces[0].actions[0],revision:undefined}]}]};
 let disk;const calls=[];
 const open=()=>createActionClient({version:2,context:()=>structuredClone(binding),load:async()=>disk,readLegacyImage:()=>JSON.stringify(disk),commit:async value=>{disk=structuredClone(value);},request:async message=>{calls.push(structuredClone(message));if(message.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:cat};return{type:'observation',host,invocation:message.invocation,update:null,uncertain:false,cancellation_requested:false};}});
 const c=await open(),ticket=await c.actionsV2.beginNew(plain,{consumer:consumer()});
 await c.actionsV2.drive(ticket);
 const sent=calls.filter(m=>m.type==='invoke_bound');assert.equal(sent.length,1);assert.equal(sent[0].invocation,ticket.operation);assert.equal(sent[0].revision,null);
 await c.close();
 const reopened=await open();await assert.rejects(reopened.actionsV2.drive(ticket),/exact target/);assert.equal(calls.filter(m=>m.type==='invoke_bound').length,1);await reopened.close();
});
test('v2 ticket accepts native value-only success output whose empty resources list is omitted',async()=>{
 const f=fixture(),c=await f.open();const a=c.actionsV2,ticket=await a.beginNew(route,{consumer:consumer()});
 f.response=m=>({type:'observation',host,invocation:m.invocation,update:{state:'succeeded',invocation:m.invocation,output:{value:{text:'invitation'}}},uncertain:false,cancellation_requested:false});
 const row=await a.observe(ticket);assert.equal(row.state,'succeeded');assert.deepEqual(row.update.output.value,{text:'invitation'});
 f.response=m=>({type:'observation',host,invocation:m.invocation,update:{state:'succeeded',invocation:m.invocation,output:{resources:[7]}},uncertain:false,cancellation_requested:false});
 const other=await a.beginNew({...route,argument:{text:'second'}},{consumer:consumer()});await assert.rejects(a.observe(other),/Invalid ticket action observation/);await c.close();
});
