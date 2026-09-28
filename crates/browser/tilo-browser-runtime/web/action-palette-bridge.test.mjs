import test from 'node:test';
import assert from 'node:assert/strict';
import {bindActionPalette} from './action-palette-bridge.mjs';
import {createActionClient} from './action-client.mjs';
const host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
const room='cccccccc-cccc-4ccc-8ccc-cccccccccccc';
const binding={room,sender:'sender',senderRoot:'root',target:'target',targetRoot:'root',certificate:'certificate'};
const catalogue={version:1,surfaces:[{peer_id:'9007199254740993',app_id:'writer',record_id:'tab:1',cluster_id:room,live:true,actions:[{resource:'document',action:'insert-table',resource_label:'Document',resource_kind:'writer',action_label:'Insert table',operation_kind:'Document',params:[],contract:{confirmation:{mode:'none'}}}]}]};
async function fixture(){
 let bytes,deny=false,resolveResponse,hold=false,currentHost=host;const currentCatalogue=structuredClone(catalogue);const sent=[];
 const actions=await createActionClient({context:()=>binding,load:async()=>bytes,commit:async value=>{if(deny)throw Error('quota');bytes=structuredClone(value);},request:async message=>{
   sent.push(message);if(message.type==='catalogue_refresh')return{type:'broker_catalogue',host:currentHost,catalogue:currentCatalogue};
   assert.ok(bytes);assert.equal(bytes.entries[0].invocation,message.invocation);
   const response={type:'observation',host:currentHost,invocation:message.invocation,update:null,uncertain:true,cancellation_requested:false};
   return hold?new Promise(resolve=>{resolveResponse=()=>resolve(response);}):response;
 }});
 await actions.catalogue();let rows=[],reports=[];
 const mount=()=>bindActionPalette({actions,publish:value=>{rows=value;},report:value=>reports.push(value)});
 return {actions,mount,sent,reports,catalogue:currentCatalogue,get rows(){return rows;},get bytes(){return bytes;},deny(){deny=true;},hold(){hold=true;},release(){resolveResponse();},host(value){currentHost=value;}};
}
test('actual client admits one exact route with full UUID after persistence; failed persistence sends no intent',async()=>{
 const f=await fixture(),bridge=f.mount();
 const control=f.rows[0].key;const row=await bridge.submit({control,argument:{text:'complete'},confirmation:null});
 const invoke=f.sent.find(m=>m.type==='invoke_bound');assert.equal(invoke.app_peer,'9007199254740993');assert.equal(invoke.record_id,'tab:1');assert.deepEqual(invoke.argument,{text:'complete'});assert.match(row.invocation,/^[0-9a-f-]{36}$/);assert.equal(row.state,'unknown');
 f.deny();await assert.rejects(bridge.submit({control}),/quota/);assert.equal(f.sent.filter(m=>m.type==='invoke_bound').length,1);bridge.close();
});
test('authority loss empties controls and old controls never retarget a replacement host',async()=>{
 const f=await fixture(),bridge=f.mount(),control=f.rows[0].key;
 f.actions.invalidate('permission-revoked');assert.equal(f.rows.length,0);await assert.rejects(bridge.submit({control}),/stale|unavailable/);
 await f.actions.catalogue();f.host('dddddddd-dddd-4ddd-8ddd-dddddddddddd');await assert.rejects(bridge.submit({control}),/binding|catalogue/);assert.equal(f.bytes,undefined);bridge.close();
});
test('replacement mount cannot claim late private observation; recovery retains the original UUID',async()=>{
 const f=await fixture(),old=f.mount();f.hold();const task=old.submit({control:f.rows[0].key});
 while(!f.sent.some(m=>m.type==='invoke_bound'))await new Promise(resolve=>setImmediate(resolve));
 const id=f.bytes.entries[0].invocation;old.close();const next=f.mount();f.release();await task;assert.equal(f.reports.length,0);
 const recovery=f.actions.status(id);while(!f.sent.some(m=>m.type==='status'))await new Promise(resolve=>setImmediate(resolve));f.release();await recovery;
 assert.equal(f.sent.filter(m=>m.type==='invoke_bound').length,1);assert.equal(f.sent.find(m=>m.type==='status').invocation,id);assert.equal(f.actions.getSnapshot().entries[0].state,'unknown');next.close();
});

test('unrelated catalogue updates preserve controls, but replacement schema invalidates the old control',async()=>{
 const f=await fixture(),bridge=f.mount(),control=f.rows[0].key;
 f.catalogue.surfaces.push({peer_id:'9',app_id:'notes',cluster_id:room,record_id:'other',actions:[]});
 await f.actions.catalogue();assert.equal(f.rows[0].key,control);
 await bridge.submit({control});assert.equal(f.sent.filter(m=>m.type==='invoke_bound').length,1);
 f.catalogue.surfaces[0].actions[0].params=[{name:'rows',ty:'number',required:true}];
 await f.actions.catalogue();await assert.rejects(bridge.submit({control}),/stale/);
 assert.equal(f.sent.filter(m=>m.type==='invoke_bound').length,1);bridge.close();
});

const deferred=()=>{let resolve;const promise=new Promise(value=>resolve=value);return{promise,resolve};};
test('mount discovers and publishes a fresh palette without another gesture',async()=>{
 let rows=[],sent=[];
 const actions=await createActionClient({context:()=>binding,load:async()=>undefined,commit:async()=>{},request:async message=>{sent.push(message);return{type:'broker_catalogue',host,catalogue};}});
 const bridge=bindActionPalette({actions,publish:value=>{rows=value;}});
 while(rows.length===0)await new Promise(resolve=>setImmediate(resolve));
 assert.equal(sent.length,1);assert.deepEqual(sent[0],{type:'catalogue_refresh'});assert.equal(rows[0].action_label,'Insert table');
 bridge.close();await actions.close();
});
test('closing one palette detaches it without cancelling another palette refresh',async()=>{
 const gate=deferred(),entered=deferred(),firstPublished=[],secondPublished=[],reports=[];
 const actions=await createActionClient({context:()=>binding,load:async()=>undefined,commit:async()=>{},request:async()=>{entered.resolve();await gate.promise;return{type:'broker_catalogue',host,catalogue};}});
 const first=bindActionPalette({actions,publish:value=>firstPublished.push(structuredClone(value)),report:value=>reports.push(value)});
 await entered.promise;const second=bindActionPalette({actions,publish:value=>secondPublished.push(structuredClone(value)),report:value=>reports.push(value)});
 first.close();gate.resolve();while(secondPublished.at(-1)?.length!==1)await new Promise(resolve=>setImmediate(resolve));
 assert.deepEqual(firstPublished,[[]]);assert.equal(secondPublished.at(-1)[0].action_label,'Insert table');assert.deepEqual(reports,[]);assert.equal(actions.getSnapshot().capability.state,'ready');
 second.close();await actions.close();
});
test('oversized general catalogue is reported as palette discovery failure',async()=>{
 const reports=[];const actions=await createActionClient({context:()=>binding,load:async()=>undefined,commit:async()=>{},request:async()=>({type:'rejected',code:'action_response_exceeds_limits'})});
 const bridge=bindActionPalette({actions,publish:()=>{},report:value=>reports.push(value)});
 while(reports.length===0)await new Promise(resolve=>setImmediate(resolve));
 assert.equal(reports[0].actionStage,'palette-discovery');assert.match(reports[0].actionError,/palette.*too much data/i);
 bridge.close();await actions.close();
});
test('v2 palette retains a ticket and consumes only after the mounted result is reported',async()=>{
 let disk,rows,reported=false;const offered=structuredClone(catalogue);offered.surfaces[0].actions[0].revision='exact';
 const client=await createActionClient({version:2,context:()=>binding,load:async()=>disk,commit:async v=>{if(v.entries.length===0&&disk?.entries.length)assert.equal(reported,true);disk=structuredClone(v);},request:async m=>m.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue:offered}:{type:'observation',host,invocation:m.invocation,update:m.type==='invoke_bound'?{state:'succeeded',invocation:m.invocation,output:{resources:[]}}:null,uncertain:false,cancellation_requested:false}});
 await client.catalogue();const bridge=bindActionPalette({actions:client.actionsV2,publish:value=>rows=value,report:()=>{reported=true;}});const row=await bridge.submit({control:rows[0].key});assert.equal(row.terminal,true);assert.equal(disk.entries.length,0);bridge.close();await client.close();
});
test('v2 repeated palette submission after reply loss drives its retained ticket instead of beginning again',async()=>{
 let disk,rows;const calls=[],offered=structuredClone(catalogue);offered.surfaces[0].actions[0].revision='exact';let recorded=false;
 const client=await createActionClient({version:2,context:()=>binding,load:async()=>disk,commit:async v=>{disk=structuredClone(v);},request:async m=>{calls.push(m);if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:offered};if(m.type==='invoke_bound'){recorded=true;throw Error('reply lost');}return{type:'observation',host,invocation:m.invocation,update:recorded?{state:'accepted',invocation:m.invocation}:null,uncertain:false,cancellation_requested:false};}});
 await client.catalogue();const bridge=bindActionPalette({actions:client.actionsV2,publish:value=>rows=value});const input={control:rows[0].key,argument:{text:'original'}};await assert.rejects(bridge.submit(input),/reply lost/);await bridge.submit(input);assert.equal(disk.entries.length,1);assert.equal(calls.filter(m=>m.type==='invoke_bound').length,1);await assert.rejects(bridge.submit({...input,argument:{text:'replacement'}}),/original|intent/);bridge.close();await client.close();
});
test('v2 palette closed during strict admission retains ticket without a late effect send',async()=>{
 let disk,rows,release,entered,held=false;const started=new Promise(r=>entered=r),calls=[],offered=structuredClone(catalogue);offered.surfaces[0].actions[0].revision='exact';
 const client=await createActionClient({version:2,context:()=>binding,load:async()=>disk,commit:async v=>{if(v.entries.length&&!held){held=true;entered();await new Promise(r=>release=r);}disk=structuredClone(v);},request:async m=>{calls.push(m);return m.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue:offered}:{type:'observation',host,invocation:m.invocation,update:null,uncertain:false,cancellation_requested:false};}});
 await client.catalogue();const bridge=bindActionPalette({actions:client.actionsV2,publish:value=>rows=value});
 const pending=bridge.submit({control:rows[0].key,argument:{text:'original'}}).then(()=>({state:'resolved'}),error=>({state:'rejected',message:error.message}));await started;bridge.close();release();await pending;await client.catalogue();
 assert.equal(disk.entries.length,1);assert.equal(disk.entries[0].ticket.consumer.slot,'palette-result');assert.equal(calls.filter(m=>m.type==='invoke_bound').length,0);await client.close();
});
test('v2 palette close while status is pending prevents subsequent absence-driven effect send',async()=>{
 let disk,rows,release,entered;const started=new Promise(r=>entered=r),gate=new Promise(r=>release=r),calls=[],offered=structuredClone(catalogue);offered.surfaces[0].actions[0].revision='exact';
 const client=await createActionClient({version:2,context:()=>binding,load:async()=>disk,commit:async v=>{disk=structuredClone(v);},request:async m=>{calls.push(m);if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:offered};if(m.type==='status'){entered();await gate;}return{type:'observation',host,invocation:m.invocation,update:null,uncertain:false,cancellation_requested:false};}});
 await client.catalogue();const bridge=bindActionPalette({actions:client.actionsV2,publish:value=>rows=value}),pending=bridge.submit({control:rows[0].key}).catch(()=>{});await started;bridge.close();release();await pending;await client.catalogue();assert.equal(disk.entries.length,1);assert.equal(calls.filter(m=>m.type==='invoke_bound').length,0);await client.close();
});
test('v2 second mounted palette cannot adopt or consume another parent by equal semantic intent',async()=>{
 let disk,firstRows,secondRows;const offered=structuredClone(catalogue);offered.surfaces[0].actions[0].revision='exact';const accepted=new Set();let finish=false;
 const client=await createActionClient({version:2,context:()=>binding,load:async()=>disk,commit:async v=>{disk=structuredClone(v);},request:async m=>{if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:offered};if(m.type==='invoke_bound')accepted.add(m.invocation);return{type:'observation',host,invocation:m.invocation,update:accepted.has(m.invocation)?finish?{state:'succeeded',invocation:m.invocation,output:{resources:[]}}:{state:'accepted',invocation:m.invocation}:null,uncertain:false,cancellation_requested:false};}});
 await client.catalogue();const first=bindActionPalette({actions:client.actionsV2,publish:value=>firstRows=value});await first.submit({control:firstRows[0].key,argument:{text:'same text'}});const original=structuredClone(disk.entries[0]);finish=true;
 const second=bindActionPalette({actions:client.actionsV2,publish:value=>secondRows=value});await second.submit({control:secondRows[0].key,argument:{text:'same text'}});assert.deepEqual(disk.entries,[original]);assert.equal(accepted.size,2,'second deliberate gesture owns a separate operation and never claims the original parent');first.close();second.close();await client.close();
});
