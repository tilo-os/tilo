import test from 'node:test';
import assert from 'node:assert/strict';
import {createAgentReadsClient} from './agent-reads-client.mjs';

const host='11111111-1111-4111-8111-111111111111', invocationId='22222222-2222-4222-8222-222222222222';
function fixture() {
  const action={room:host,sender:'ab'.repeat(32),senderRoot:'cd'.repeat(32),target:'ef'.repeat(32),targetRoot:'01'.repeat(32),certificate:'certificate'};
  const agent={machine:'machine',room:host,device:action.sender,host};
  const descriptor={resource:'workspace:agents',action:'spawn',revision:'r1'};
  const spawn={route:{app_peer:'1',app_id:'agent',record_id:'',resource:descriptor.resource,action:'spawn',revision:'r1'},descriptor};
  const binding={actionHost:host,agent,spawn,message:null};
  let capability={state:'ready',host,binding:structuredClone(action),catalogue:{version:1,agentReadsV1:binding,surfaces:[{peer_id:'1',app_id:'agent',record_id:'',cluster_id:host,live:true,actions:[descriptor]}]}};
  const listeners=new Set(),pending=[];let supported=true,now=0,nextTimer=0;const timers=new Map();
  const client=createAgentReadsClient({actions:{getSnapshot:()=>({capability}),subscribe:fn=>{listeners.add(fn);return()=>listeners.delete(fn);}},context:()=>action,supported:()=>supported,
    request:(message,{signal})=>new Promise((resolve,reject)=>{pending.push({message,signal,resolve,reject});signal.addEventListener('abort',()=>reject(signal.reason),{once:true});}),
    clock:{now:()=>now,setTimeout:(fn,ms)=>{const id=++nextTimer;timers.set(id,{fn,at:now+ms});return id;},clearTimeout:id=>timers.delete(id)}});
  const response=(index=0)=>{const request=pending[index].message;return {type:'agent_read_result_v1',host,invocation:request.invocation,binding:structuredClone(binding),response:{kind:request.request.kind,version:1,invocationId:request.invocation,binding:structuredClone(agent),agents:[],nextCursor:null,revision:'1'}};};
  return {client,action,binding,pending,timers,listeners,response,change(value){capability=value;for(const fn of listeners)fn();},unsupported(){supported=false;},advance(ms){now+=ms;for(const [id,timer] of [...timers])if(timer.at<=now){timers.delete(id);timer.fn();}},capability:()=>capability};
}

test('copies the certified current binding and sends a disposable read without storage',async()=>{
  const f=fixture();assert.equal(f.client.version,1);
  const binding=f.client.getBinding();assert.deepEqual(binding,{...f.binding,action:f.action});
  binding.agent.machine='forged';assert.equal(f.client.getBinding().agent.machine,'machine');
  const p=f.client.request({invocationId,request:{version:1,kind:'agent_list',cursor:null}});
  assert.equal(f.pending.length,1);assert.deepEqual(f.pending[0].message,{type:'agent_read_v1',host,invocation:invocationId,request:{version:1,kind:'agent_list',cursor:null}});
  f.pending[0].resolve(f.response());assert.equal((await p).kind,'agent_list');assert.equal(f.timers.size,0);f.client.close();assert.equal(f.listeners.size,0);
});
test('withholds unsupported, stale and ambiguous provider bindings',()=>{
  for(const mutate of [f=>f.unsupported(),f=>{f.action.targetRoot='ff'.repeat(32);},f=>{f.capability().catalogue.surfaces.push(structuredClone(f.capability().catalogue.surfaces[0]));},f=>{f.binding.agent.device='ff'.repeat(32);},f=>{f.binding.spawn.descriptor.revision='other';},f=>{delete f.capability().catalogue.agentReadsV1;}]) {
    const f=fixture();mutate(f);assert.throws(()=>f.client.getBinding());f.client.close();
  }
});
test('rejects conflicting duplicate route candidates before comparing provider descriptors',async()=>{
  for(const conflict of [{description:'conflicting description'},{contract:{kind:'conflicting contract'}}]){
    const f=fixture(),duplicate=structuredClone(f.capability().catalogue.surfaces[0]);
    Object.assign(duplicate.actions[0],conflict);
    f.capability().catalogue.surfaces.push(duplicate);
    assert.throws(()=>f.client.getBinding(),/ambiguous or stale provider route/);
    await assert.rejects(f.client.request({invocationId,request:{version:1,kind:'agent_list',cursor:null}}),/ambiguous or stale provider route/);
    assert.equal(f.pending.length,0);assert.equal(f.timers.size,0);f.client.close();
  }
});
test('rejects mutations, status, malformed requests and duplicate correlations before sending',async()=>{
  const f=fixture();
  for(const request of [{version:1,kind:'agent_spawn'}, {version:1,kind:'agent_message'},{version:1,kind:'agent_status'},{version:2,kind:'agent_list',cursor:null},{version:1,kind:'agent_list'},{version:1,kind:'agent_list',cursor:null,extra:true}])await assert.rejects(f.client.request({invocationId,request}));
  assert.equal(f.pending.length,0);
  const first=f.client.request({invocationId,request:{version:1,kind:'agent_list',cursor:null}});const rejected=assert.rejects(first);
  await assert.rejects(f.client.request({invocationId,request:{version:1,kind:'agent_list',cursor:null}}));assert.equal(f.pending.length,1);f.client.close();await rejected;
});
test('abort, deadline, invalidation and close settle reads and clear transport controls',async()=>{
  for(const cancel of [(f,c)=>c.abort(),f=>f.advance(10000),f=>f.client.invalidate('reconnecting'),f=>f.client.close(),f=>f.change({state:'unavailable'})]){
    const f=fixture(),controller=new AbortController();const p=f.client.request({invocationId,request:{version:1,kind:'agent_list',cursor:null},signal:controller.signal});const rejected=assert.rejects(p);cancel(f,controller);await rejected;
    assert.equal(f.pending[0].signal.aborted,true);assert.equal(f.timers.size,0);f.pending[0].resolve(f.response());f.client.close();
  }
});
test('rejects mismatched host, response binding, read kind and provider revision',async()=>{
  for(const mutate of [r=>{r.host='33333333-3333-4333-8333-333333333333';},r=>{r.response.binding.device='ff'.repeat(32);},r=>{r.response.invocationId='other';},r=>{r.response.kind='agent_operation';},r=>{r.binding.agent.machine='other';}]){
    const f=fixture();const p=f.client.request({invocationId,request:{version:1,kind:'agent_list',cursor:null}});const rejected=assert.rejects(p);const response=f.response();mutate(response);f.pending[0].resolve(response);await rejected;assert.equal(f.timers.size,0);f.client.close();
  }
});
test('limits pending reads and rejects a response crossing a context change',async()=>{
  const f=fixture(),ps=[];
  for(let i=1;i<=32;i++)ps.push(assert.rejects(f.client.request({invocationId:`22222222-2222-4222-8222-${String(i).padStart(12,'0')}`,request:{version:1,kind:'agent_list',cursor:null}})));
  await assert.rejects(f.client.request({invocationId:'33333333-3333-4333-8333-333333333333',request:{version:1,kind:'agent_list',cursor:null}}));assert.equal(f.pending.length,32);f.action.target='00'.repeat(32);f.pending[0].resolve(f.response());await ps[0];f.client.close();await Promise.all(ps);assert.equal(f.timers.size,0);
});
