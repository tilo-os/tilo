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
