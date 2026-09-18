import test from 'node:test';
import assert from 'node:assert/strict';
import {bindDesktopCommands} from './desktop-command-bridge.mjs';
import {createActionClient} from './action-client.mjs';
const host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',room='cccccccc-cccc-4ccc-8ccc-cccccccccccc';
const binding={room,sender:'sender',senderRoot:'root',target:'target',targetRoot:'root',certificate:'certificate'};
const invocation='eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee';
// Deliberately beyond JS integer precision: the bridge must never decode/rebuild it.
const command='{"source":{"room":"'+room+'","heads":[[7]]},"command":{"CloseTab":{"location":{"strip":18446744073709551001,"group":18446744073709551002,"tab":18446744073709551003}}}}';
const catalogue={version:1,surfaces:[{peer_id:'9007199254740993',app_id:'dev.qos.desktop',record_id:'',cluster_id:room,live:true,actions:[{resource:'desktop:session',action:'command',resource_label:'Desktop',resource_kind:'desktop',action_label:'Change desktop',operation_kind:'Effect',params:[{name:'command',ty:'text',required:true}],contract:{confirmation:{mode:'none'}}}]}]};
async function fixture(){
 let disk,deny=false,available=true,outcome='unknown';const sent=[],reports=[];let queue=[{invocation,command}];
 const config={context:()=>binding,load:async()=>disk,commit:async value=>{if(deny)throw Error('quota');disk=structuredClone(value);},request:async message=>{
  sent.push(message);if(message.type==='catalogue_refresh')return{type:'broker_catalogue',host,catalogue:available?catalogue:{version:1,surfaces:[]}};
  assert.equal(disk.entries[0].invocation,message.invocation,'dispatch follows durable admission');
  if(outcome==='succeeded')return{type:'observation',host,invocation:message.invocation,update:{state:'succeeded',invocation:message.invocation,output:{resources:[]}},uncertain:false,cancellation_requested:false};
  if(outcome==='failed')return{type:'observation',host,invocation:message.invocation,update:{state:'failed',invocation:message.invocation,error:{code:'refused',message:'refused',retryable:false}},uncertain:false,cancellation_requested:false};
  return {type:'observation',host,invocation:message.invocation,update:null,uncertain:true,cancellation_requested:false};
 }};
 const actions=await createActionClient(config);
 const bridge=bindDesktopCommands({actions,peek:()=>queue[0],ack:id=>{assert.equal(queue[0].invocation,id);queue.shift();},report:value=>reports.push(value)});
 return {actions,bridge,sent,reports,config,get disk(){return disk;},get queue(){return queue;},deny(){deny=true;},unavailable(){available=false;},succeed(){outcome='succeeded';},fail(){outcome='failed';}};
}
test('widget command preserves full IDs and UUID through actual durable client and restart recovery',async()=>{
 const f=await fixture();await f.bridge.pump();
 const invoke=f.sent.find(row=>row.type==='invoke_bound');assert.equal(invoke.invocation,invocation);assert.equal(invoke.argument.command,command);assert.equal(invoke.app_peer,'9007199254740993');assert.equal(f.queue.length,0);assert.equal(f.disk.entries.length,1);
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
