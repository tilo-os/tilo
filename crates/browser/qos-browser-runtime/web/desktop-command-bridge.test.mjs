import test from 'node:test';
import assert from 'node:assert/strict';
import {bindDesktopCommands} from './desktop-command-bridge.mjs';
import {createActionClient} from './action-client.mjs';
const host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',room='cccccccc-cccc-4ccc-8ccc-cccccccccccc';
const binding={room,sender:'sender',senderRoot:'root',target:'target',targetRoot:'root',certificate:'certificate'};
const invocation='eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee';
// Deliberately beyond JS integer precision: the bridge must never decode/rebuild it.
const command='{"source":{"room":"'+room+'","heads":[[7]]},"command":{"CloseTab":{"location":{"strip":18446744073709551001,"group":18446744073709551002,"tab":18446744073709551003}}}}';
const catalogue={version:1,surfaces:[{peer_id:'9007199254740993',app_id:'dev.qos.desktop',record_id:'',cluster_id:room,live:true,actions:[{resource:'desktop:session',action:'command',resource_label:'Desktop',resource_kind:'desktop',action_label:'Change desktop',operation_kind:'Effect',params:[{name:'command',ty:'text',required:true},{name:'committed_desktop_v2',ty:'boolean',required:false}],contract:{confirmation:{mode:'none'}}}]}]};
async function fixture({requestHook,commitHook,clock}={}){
 let disk,deny=false,available=true,outcome='unknown';const sent=[],reports=[];let queue=[{invocation,command}];
 const config={context:()=>binding,load:async()=>disk,commit:async value=>{await commitHook?.(value);if(deny)throw Error('quota');disk=structuredClone(value);},request:async message=>{
  sent.push(message);const override=await requestHook?.(message,sent);if(override!==undefined)return override;if(message.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:available?catalogue:{version:1,surfaces:[]}};
  assert.equal(disk.entries[0].invocation,message.invocation,'dispatch follows durable admission');
  if(outcome==='succeeded')return{type:'observation',host,invocation:message.invocation,update:{state:'succeeded',invocation:message.invocation,output:{resources:[]}},uncertain:false,cancellation_requested:false};
  if(outcome==='failed')return{type:'observation',host,invocation:message.invocation,update:{state:'failed',invocation:message.invocation,error:{code:'refused',message:'refused',retryable:false}},uncertain:false,cancellation_requested:false};
  return {type:'observation',host,invocation:message.invocation,update:null,uncertain:true,cancellation_requested:false};
 }};
 const actions=await createActionClient({...config,clock});
 const bridge=bindDesktopCommands({actions,clock,peek:()=>queue[0],ack:id=>{assert.equal(queue[0].invocation,id);queue.shift();},report:value=>reports.push(value)});
 return {actions,bridge,sent,reports,config,get disk(){return disk;},get queue(){return queue;},deny(){deny=true;},unavailable(){available=false;},succeed(){outcome='succeeded';},fail(){outcome='failed';}};
}
test('widget command preserves full IDs and UUID through actual durable client and restart recovery',async()=>{
 const f=await fixture();await f.bridge.pump();
 const invoke=f.sent.find(row=>row.type==='invoke_bound');assert.equal(invoke.invocation,invocation);assert.equal(invoke.argument.command,command);assert.equal(invoke.argument.committed_desktop_v2,true);assert.equal(invoke.app_peer,'9007199254740993');assert.equal(f.queue.length,0);assert.equal(f.disk.entries.length,1);
 assert.equal(f.reports.at(-1).desktopCommand.state,'unknown');
 f.bridge.close();const recovered=await createActionClient(f.config);await recovered.catalogue();await recovered.recover();
 assert.equal(f.sent.filter(row=>row.type==='invoke_bound').length,1);assert.equal(f.sent.find(row=>row.type==='status').invocation,invocation);
});
test('storage failure visibly refuses before any dispatch; unavailable service does not claim success',async()=>{
 for(const failure of ['deny','unavailable']){const f=await fixture();f[failure]();await f.bridge.pump();assert.equal(f.sent.filter(row=>row.type==='invoke_bound').length,0);assert.equal(f.queue.length,0);assert.equal(f.reports.at(-1).desktopCommand.state,'rejected');assert.ok(f.reports.at(-1).desktopCommand.error);f.bridge.close();}
});
test('terminal waiter resolves success, rejects failure, and retains unknown invocation',async()=>{
 const f=await fixture();
 const pending=f.bridge.waitForTerminal(invocation);await f.bridge.pump();
 let settled=false;pending.then(()=>{settled=true;});await Promise.resolve();assert.equal(settled,false,'unknown outcome stays pending');
 f.succeed();await f.actions.status(invocation);await pending;assert.equal(settled,true);f.bridge.close();
 const rejected=await fixture();const refused=rejected.bridge.waitForTerminal(invocation);rejected.fail();await rejected.bridge.pump();await assert.rejects(refused,/refused/);rejected.bridge.close();
 const unknown=await fixture();const retained=unknown.bridge.waitForTerminal(invocation);await unknown.bridge.pump();unknown.bridge.close();await assert.rejects(retained,/closed/);
});

const deferred=()=>{let resolve;const promise=new Promise(r=>resolve=r);return{promise,resolve};};
const drain=async()=>{for(let i=0;i<50;i++)await Promise.resolve();};
function fakeClock(){let time=0,id=0;const timers=new Map();return{now:()=>time,setTimeout:(fn,delay)=>{timers.set(++id,{fn,at:time+delay});return id;},clearTimeout:id=>timers.delete(id),async tick(ms){const end=time+ms;await drain();for(;;){const due=[...timers].filter(([,t])=>t.at<=end).sort((a,b)=>a[1].at-b[1].at)[0];if(!due)break;time=due[1].at;timers.delete(due[0]);due[1].fn();await drain();}time=end;await drain();}};}
const delayed=()=>Object.assign(Error('discovery delayed'),{code:'transport-timeout',stage:'discovery',retryable:true});
test('catalogue timeout then success retains and launches the exact original intent once',async()=>{
 const clock=fakeClock();let calls=0;const f=await fixture({clock,requestHook:m=>{if(m.type==='catalogue_refresh'&&++calls===1)throw delayed();}});
 await f.bridge.pump();assert.equal(f.queue.length,1);assert.equal(f.reports.at(-1).desktopCommand.terminal,false);
 await clock.tick(249);assert.equal(calls,1);await clock.tick(1);assert.equal(calls,2);assert.equal(f.sent.filter(m=>m.type==='invoke_bound').length,1);assert.equal(f.disk.entries[0].intent.argument.command,command);assert.equal(f.queue.length,0);f.bridge.close();
});
test('close, explicit cancel and actual queue supersession prevent late discovery dispatch',async()=>{
 for(const stop of ['close','cancel','supersede']){const hold=deferred(),entered=deferred();const f=await fixture({requestHook:async m=>{if(m.type==='catalogue_refresh'){entered.resolve();await hold.promise;}}});
 const pumping=f.bridge.pump();await entered.promise;if(stop==='close')f.bridge.close();else if(stop==='cancel')f.bridge.cancel?.(invocation);else f.queue[0]={invocation,command:command+' '};
 const reports=f.reports.length;hold.resolve();await pumping;assert.equal(f.sent.filter(m=>m.type==='invoke_bound').length,0);if(stop==='close')assert.equal(f.reports.length,reports);f.bridge.close();}
});
test('repeated pumps cannot reset retry count or lifetime for unavailable discovery',async()=>{
 const clock=fakeClock(),f=await fixture({clock,requestHook:m=>{if(m.type==='catalogue_refresh')throw delayed();}});
 await f.bridge.pump();for(let i=0;i<10;i++)await f.bridge.pump();assert.equal(f.sent.length,1);await clock.tick(1250);assert.equal(f.sent.length,3);assert.equal(f.queue.length,0);assert.match(f.reports.at(-1).desktopCommand.error,/not sent/i);await clock.tick(60000);assert.equal(f.sent.length,3);f.bridge.close();
});
test('lost response after dispatch remains unknown and recovers status under original UUID',async()=>{
 const f=await fixture({requestHook:m=>{if(m.type==='invoke_bound')throw delayed();}});await f.bridge.pump();assert.equal(f.queue.length,0);assert.equal(f.reports.at(-1).desktopCommand.state,'unknown');await f.bridge.pump();await f.actions.recover();const reopened=await createActionClient(f.config);await reopened.recover();assert.equal(f.sent.filter(m=>m.type==='invoke_bound').length,1);assert.deepEqual(f.sent.filter(m=>m.type==='status').map(m=>m.invocation),[invocation,invocation]);f.bridge.close();await reopened.close();
});
test('first failed discovery pins identity across retries',async()=>{
 const clock=fakeClock();let calls=0,current={...binding};const sent=[];let queue=[{invocation,command}];
 const actions=await createActionClient({clock,context:()=>current,load:async()=>undefined,commit:async()=>{},request:async m=>{sent.push(m);if(m.type==='catalogue_refresh'){if(++calls===1)throw delayed();return{type:'broker_catalogue',host,catalogue};}return{type:'observation',host,invocation,update:null,uncertain:true};}});
 const bridge=bindDesktopCommands({actions,clock,peek:()=>queue[0],ack:()=>queue.shift()});await bridge.pump();current={...binding,certificate:'replacement'};await clock.tick(250);assert.equal(sent.filter(m=>m.type==='invoke_bound').length,0);assert.equal(queue.length,0);bridge.close();await actions.close();
});
test('deadline includes queued discovery and a late response cannot dispatch',async()=>{
 const clock=fakeClock(),hold=deferred();let calls=0;const f=await fixture({clock,requestHook:async m=>{if(m.type==='catalogue_refresh'&&++calls===1)await hold.promise;}});const pumping=f.bridge.pump();await drain();await clock.tick(30000);await pumping;assert.equal(f.queue.length,0);assert.match(f.reports.at(-1).desktopCommand.error,/not sent/i);hold.resolve();await drain();assert.equal(f.sent.filter(m=>m.type==='invoke_bound').length,0);assert.equal(f.disk,undefined);f.bridge.close();
});
test('deadline during pending commit never retries or dispatches and retains original row',async()=>{
 const clock=fakeClock(),hold=deferred(),entered=deferred();const f=await fixture({clock,commitHook:async()=>{entered.resolve();await hold.promise;}});const pumping=f.bridge.pump();await entered.promise;await clock.tick(30000);assert.equal(f.actions.getInvocationStage(invocation),'admitting');assert.equal(f.queue.length,1);assert.equal(f.reports.at(-1).desktopCommand.state,'unknown');hold.resolve();await pumping;assert.equal(f.queue.length,0);assert.equal(f.disk.entries[0].invocation,invocation);assert.equal(f.sent.filter(m=>m.type==='invoke_bound').length,0);assert.equal(f.reports.at(-1).desktopCommand.state,'unknown');f.bridge.close();
});
test('fresh desktop selection refuses unavailable routes and required confirmation before admission',async()=>{
 for(const change of ['live','cluster','removed','confirmation','permission','expired']){
  const clock=fakeClock();let calls=0;const f=await fixture({clock,requestHook:m=>{
   if(m.type!=='catalogue_refresh')return;calls++;const changed=structuredClone(catalogue);
   if(change==='permission'||change==='expired')throw Error(change==='expired'?'device cert has expired':'permission revoked');
   if(change==='live')changed.surfaces[0].live=false;
   if(change==='cluster')changed.surfaces[0].cluster_id='wrong';
   if(change==='removed')changed.surfaces=[];
   if(change==='confirmation')changed.surfaces[0].actions[0].contract.confirmation={mode:'confirm'};
   return {type:'broker_catalogue',host,catalogue:changed};
  }});
  await f.bridge.pump();await clock.tick(30000);
  assert.equal(f.sent.filter(m=>m.type==='invoke_bound').length,0,change);
  assert.equal(f.disk,undefined,change);assert.equal(calls,1,change);
  assert.equal(f.reports.at(-1).desktopCommand.state,'rejected',change);
  f.bridge.close();await f.actions.close();
 }
});
test('supersession during backoff rejects the retired intent waiter',async()=>{
 const clock=fakeClock(),f=await fixture({clock,requestHook:m=>{if(m.type==='catalogue_refresh')throw delayed();}});const waiting=f.bridge.waitForTerminal(invocation);await f.bridge.pump();f.queue[0]={invocation:'ffffffff-ffff-4fff-8fff-ffffffffffff',command};let outcome='pending';waiting.then(()=>outcome='success',()=>outcome='cancelled');await f.bridge.pump();await drain();assert.equal(outcome,'cancelled');f.bridge.close();
});
test('explicit cancellation retires a queued intent before its first pump',async()=>{
 const f=await fixture(),waiting=f.bridge.waitForTerminal(invocation),rejected=assert.rejects(waiting,/cancelled/);assert.equal(f.bridge.cancel(invocation),true);await rejected;await f.bridge.pump();assert.equal(f.sent.length,0);assert.equal(f.queue.length,0);f.bridge.close();
});


test('desktop open and close use one fresh catalogue round trip before dispatch',async()=>{
 const f=await fixture();await f.bridge.pump();
 assert.deepEqual(f.sent.map(m=>m.type),['catalogue_refresh','invoke_bound']);
 assert.deepEqual(f.sent[0],{type:'catalogue_refresh',scope:{app_id:'dev.qos.desktop',record_id:'',resource:'desktop:session',action:'command'}});
 f.bridge.close();await f.actions.close();
});
test('legacy oversized discovery refusal is a clear nonretryable no-send error',async()=>{
 const f=await fixture({requestHook:m=>m.type==='catalogue_refresh'?{type:'rejected',code:'action_response_exceeds_limits'}:undefined});
 await f.bridge.pump();
 assert.equal(f.sent.filter(m=>m.type==='invoke_bound').length,0);
 assert.match(f.reports.at(-1).desktopCommand.error,/returned too much data; command was not sent/);
 assert.equal(f.queue.length,0);
 f.bridge.close();await f.actions.close();
});
test('command presentation starts before any network request and observes every retained completion',async()=>{
 const f=await fixture(),events=[];
 f.bridge.close();
 const bridge=bindDesktopCommands({actions:f.actions,peek:()=>f.queue[0],ack:()=>f.queue.shift(),onPending:pending=>events.push(['pending',pending]),onUpdate:row=>events.push(['update',row])});
 const pumping=bridge.pump();assert.equal(events[0]?.[0],'pending');assert.equal(events[0]?.[1].command,command);
 await pumping;f.succeed();await f.actions.status(invocation);
 assert(events.some(([kind,row])=>kind==='update'&&row.invocation===invocation&&row.terminal&&row.state==='succeeded'));
 bridge.close();await f.actions.close();
});


test('supersession clears presentation both during discovery and while backing off',async()=>{
 for(const busy of [true,false]){
  const gate=deferred(),entered=deferred(),clock=fakeClock(),updates=[];
  const f=await fixture({clock,requestHook:async m=>{if(m.type==='catalogue_refresh'){entered.resolve();if(busy)await gate.promise;else throw delayed();}}});
  f.bridge.close();const bridge=bindDesktopCommands({actions:f.actions,clock,peek:()=>f.queue[0],ack:()=>f.queue.shift(),onUpdate:row=>updates.push(row)});
  const pumping=bridge.pump();await entered.promise;if(!busy)await pumping;
  f.queue[0]={invocation:'ffffffff-ffff-4fff-8fff-ffffffffffff',command};
  if(busy){gate.resolve();await pumping;}else await bridge.pump();
  assert(updates.some(row=>row.invocation===invocation&&row.state==='cancelled'&&row.terminal));
  bridge.close();await f.actions.close();
 }
});

test('older server catalogue is used without an unsupported direct-delivery argument',async()=>{
 const old=structuredClone(catalogue);old.surfaces[0].actions[0].params=old.surfaces[0].actions[0].params.filter(p=>p.name!=='committed_desktop_v2');
 const f=await fixture({requestHook:async message=>message.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue:old}:undefined});
 try { await f.bridge.pump();assert.equal(f.sent.find(m=>m.type==='invoke_bound').argument.committed_desktop_v2,undefined); }
 finally { f.bridge.close(); }
});
test('v2 desktop maps presentation parent to an internally allocated ticket before queue acknowledgement',async()=>{
 let disk,pending={invocation,command},acknowledged=false;const calls=[],updates=[];const offered=structuredClone(catalogue);offered.surfaces[0].actions[0].revision='exact';
 const client=await createActionClient({version:2,context:()=>binding,load:async()=>disk,commit:async value=>{disk=structuredClone(value);},request:async m=>{calls.push(m);if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:offered};return{type:'observation',host,invocation:m.invocation,update:m.type==='invoke_bound'?{state:'succeeded',invocation:m.invocation,output:{resources:[]}}:null,uncertain:false,cancellation_requested:false};}});
 const bridge=bindDesktopCommands({actions:client.actionsV2,peek:()=>pending,ack:id=>{assert.equal(id,invocation);assert.equal(disk.entries[0].ticket.consumer.parent,invocation);assert.equal(calls.filter(m=>m.type==='invoke_bound').length,0);pending=undefined;acknowledged=true;},onUpdate:row=>updates.push(row)});
 await bridge.pump();assert.equal(acknowledged,true);assert.equal(calls.filter(m=>m.type==='invoke_bound').length,1);assert.notEqual(calls.find(m=>m.type==='invoke_bound').invocation,invocation);assert.ok(updates.some(row=>row.invocation===invocation&&row.terminal));
 await bridge.waitForTerminal(invocation);await drain();assert.equal(disk.entries.length,0,'presented terminal is explicitly acknowledged then consumed');bridge.close();await client.close();
});
test('v2 queue supersession during strict admission retains original ticket and sends no effect',async()=>{
 let disk,pending={invocation,command},release,entered,blocked=false;const held=new Promise(r=>entered=r),calls=[],offered=structuredClone(catalogue);offered.surfaces[0].actions[0].revision='exact';
 const client=await createActionClient({version:2,context:()=>binding,load:async()=>disk,commit:async value=>{if(value.entries.length&&!blocked){blocked=true;entered();await new Promise(r=>release=r);}disk=structuredClone(value);},request:async m=>{calls.push(m);return m.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue:offered}:{type:'observation',host,invocation:m.invocation,update:null,uncertain:false,cancellation_requested:false};}});
 const bridge=bindDesktopCommands({actions:client.actionsV2,peek:()=>pending,ack:()=>{pending=undefined;}}),pump=bridge.pump();await held;pending={invocation:'ffffffff-ffff-4fff-8fff-ffffffffffff',command:'replacement'};release();await pump;assert.equal(disk.entries.length,1);assert.equal(calls.filter(m=>m.type==='invoke_bound').length,0);bridge.close();await client.close();
});
test('v2 unrelated desktop mount cannot acknowledge another presentation parent terminal',async()=>{
 let disk;const offered=structuredClone(catalogue);offered.surfaces[0].actions[0].revision='exact';
 const client=await createActionClient({version:2,context:()=>binding,load:async()=>disk,commit:async v=>{disk=structuredClone(v);},request:async m=>m.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue:offered}:{type:'observation',host,invocation:m.invocation,update:{state:'succeeded',invocation:m.invocation,output:{resources:[]}},uncertain:false,cancellation_requested:false}});
 const ticket=await client.actionsV2.beginNew({app_peer:'9007199254740993',app_id:'dev.qos.desktop',record_id:'',resource:'desktop:session',action:'command',revision:'exact',argument:{command}},{consumer:{kind:'interactive',parent:invocation,slot:'desktop-command'}});
 const unrelated=bindDesktopCommands({actions:client.actionsV2,peek:()=>undefined,ack:()=>{},onUpdate:()=>{}});await client.actionsV2.observe(ticket);await drain();assert.equal(disk.entries.length,1);assert.equal(disk.entries[0].consumerAcknowledgement,null);unrelated.close();await client.close();
});
test('v2 frame pump after queue acknowledgement does not cancel the ticket drive before its original dispatch',async()=>{
 // host.mjs pumps on every frame; ack() removes the queue head and requests a
 // redraw while drive() still awaits status. That pump must not abort the drive.
 let disk,pending={invocation,command},bridge,statusEntered,releaseStatus;const calls=[],updates=[],entered=new Promise(r=>statusEntered=r),offered=structuredClone(catalogue);
 const client=await createActionClient({version:2,context:()=>binding,load:async()=>disk,commit:async value=>{disk=structuredClone(value);},request:async m=>{calls.push(m);
  if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:offered};
  // A fresh ticket's first dispatch goes out directly (no status probe); hold that dispatch.
  if(m.type==='invoke_bound'&&!releaseStatus){statusEntered();await new Promise(r=>releaseStatus=r);}
  return{type:'observation',host,invocation:m.invocation,update:m.type==='invoke_bound'?{state:'succeeded',invocation:m.invocation,output:{resources:[]}}:null,uncertain:false,cancellation_requested:false};}});
 bridge=bindDesktopCommands({actions:client.actionsV2,peek:()=>pending,ack:()=>{pending=undefined;},onUpdate:row=>updates.push(row)});
 const terminal=bridge.waitForTerminal(invocation),first=bridge.pump();await entered;
 await bridge.pump();releaseStatus();await first;
 assert.equal(calls.filter(m=>m.type==='invoke_bound').length,1,'the admitted ticket reaches its single original dispatch');
 await terminal;assert.ok(updates.some(row=>row.invocation===invocation&&row.terminal&&row.state==='succeeded'));
 bridge.close();await client.close();
});
test('v2 supersession before queue acknowledgement still cancels the unadmitted attempt',async()=>{
 let disk,pending={invocation,command},releaseCatalogue,entered;const held=new Promise(r=>entered=r),calls=[],offered=structuredClone(catalogue);
 const client=await createActionClient({version:2,context:()=>binding,load:async()=>disk,commit:async value=>{disk=structuredClone(value);},request:async m=>{calls.push(m);
  if(m.type==='catalogue_refresh'&&!releaseCatalogue){entered();await new Promise(r=>releaseCatalogue=r);}
  return m.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue:offered}:{type:'observation',host,invocation:m.invocation,update:null,uncertain:false,cancellation_requested:false};}});
 const bridge=bindDesktopCommands({actions:client.actionsV2,peek:()=>pending,ack:()=>{pending=undefined;}}),first=bridge.pump();await held;
 pending={invocation:'ffffffff-ffff-4fff-8fff-ffffffffffff',command:'replacement'};await bridge.pump();releaseCatalogue();await first;
 assert.equal(disk?.entries.length??0,0,'no ticket is admitted for the superseded command');assert.equal(calls.filter(m=>m.type==='invoke_bound').length,0);
 bridge.close();await client.close();
});
test('a fresh v2 desktop command costs one scoped discovery and its dispatch, nothing else',async()=>{
 let disk,pending={invocation,command};const calls=[],offered=structuredClone(catalogue);
 const client=await createActionClient({version:2,context:()=>binding,load:async()=>disk,commit:async value=>{disk=structuredClone(value);},request:async m=>{calls.push(m);
  if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:offered};
  return{type:'observation',host,invocation:m.invocation,update:m.type==='invoke_bound'?{state:'succeeded',invocation:m.invocation,output:{resources:[]}}:null,uncertain:false,cancellation_requested:false};}});
 const bridge=bindDesktopCommands({actions:client.actionsV2,peek:()=>pending,ack:()=>{pending=undefined;}});
 const terminal=bridge.waitForTerminal(invocation);await bridge.pump();await terminal;
 assert.deepEqual(calls.map(m=>m.type),['catalogue_refresh','invoke_bound']);
 assert.deepEqual(calls[0].scope,{app_id:'dev.qos.desktop',record_id:'',resource:'desktop:session',action:'command'},'the discovery is scoped to the desktop command');
 bridge.close();await client.close();
});
test('a desktop open is sent at once while an agent spawn ticket waits on a host that never answers',async()=>{
 let disk,pending;const calls=[];const offered=structuredClone(catalogue);
 offered.surfaces.push({peer_id:'12',app_id:'so.tilo.agent',record_id:'',cluster_id:room,live:true,actions:[{resource:'agents',action:'spawn',revision:'r1',contract:{confirmation:{mode:'none'}}}]});
 const stuck=new Set();
 const client=await createActionClient({version:2,context:()=>binding,load:async()=>disk,commit:async value=>{disk=structuredClone(value);},request:async (m,control)=>{calls.push(m);
  if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:offered};
  // The agent service never answers this spawn (a lost reply): only the caller's deadline ends the wait.
  if(stuck.has(m.invocation))return new Promise((_resolve,reject)=>control.signal.addEventListener('abort',()=>reject(control.signal.reason),{once:true}));
  return{type:'observation',host,invocation:m.invocation,update:m.type==='invoke_bound'?{state:'succeeded',invocation:m.invocation,output:{resources:[]}}:null,uncertain:false,cancellation_requested:false};}});
 const spawn=await client.actionsV2.beginNew({app_peer:'12',app_id:'so.tilo.agent',record_id:'',resource:'agents',action:'spawn',revision:'r1',argument:{name:'a'},confirmation:null},{consumer:{kind:'interactive',parent:crypto.randomUUID(),slot:'agent-spawn'}});
 stuck.add(spawn.operation);
 const abort=new AbortController();const hung=client.actionsV2.drive(spawn,{signal:abort.signal});hung.catch(()=>{});
 await new Promise(r=>setTimeout(r,20));
 assert.ok(calls.some(m=>m.invocation===spawn.operation),'the spawn is waiting on the host');
 pending={invocation,command};
 const bridge=bindDesktopCommands({actions:client.actionsV2,peek:()=>pending,ack:()=>{pending=undefined;}});
 const opened=bridge.waitForTerminal(invocation);
 const began=Date.now();await bridge.pump();await opened;
 assert.ok(Date.now()-began<1000,'the open was not serialized behind the stuck spawn');
 assert.equal(calls.filter(m=>m.type==='invoke_bound'&&m.app_id==='dev.qos.desktop').length,1);
 abort.abort();bridge.close();await client.close();
});
