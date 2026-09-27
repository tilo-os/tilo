import test from 'node:test';
import assert from 'node:assert/strict';
import {createDesktopSpeculation,CONFIRMED_HOLD_MS,UNCONFIRMED_DEADLINE_MS} from './desktop-speculation.mjs';

function fakeClock(){let now=0,next=1;const timers=new Map();return {
  setTimeout:(fn,ms)=>{const id=next++;timers.set(id,{fn,at:now+ms});return id;},clearTimeout:id=>timers.delete(id),
  advance(ms){now+=ms;for(const [id,t] of [...timers].sort((a,b)=>a[1].at-b[1].at))if(t.at<=now){timers.delete(id);t.fn();}},size:()=>timers.size};}
function fakeRuntime(){const calls=[];return {calls,desktop_speculation_enable:(id,on)=>calls.push(['enable',id,on]),desktop_command_settled:(id,inv,outcome,receipt)=>{calls.push(receipt===undefined?['settled',id,inv,outcome]:['settled',id,inv,outcome,JSON.parse(receipt)]);return true;}};}

test('enables runtime speculation and settles terminal outcomes', () => {
  const runtime=fakeRuntime(),clock=fakeClock();
  const spec=createDesktopSpeculation({runtime,id:7,active:()=>true,clock});
  assert.equal(spec.available,true);
  assert.deepEqual(runtime.calls,[['enable',7,true]]);
  spec.report({desktopCommand:{invocation:'a',state:'submitting',terminal:false}});
  spec.report({desktopCommand:{invocation:'a',state:'unknown',terminal:false}});
  assert.equal(runtime.calls.length,1,'non-terminal reports keep the speculation');
  spec.report({desktopCommand:{invocation:'a',state:'rejected',terminal:true,error:'x'}});
  assert.deepEqual(runtime.calls.at(-1),['settled',7,'a',false]);
  spec.report({desktopCommand:{invocation:'b',state:'succeeded',terminal:true}});
  assert.deepEqual(runtime.calls.at(-1),['settled',7,'b',true]);
  clock.advance(CONFIRMED_HOLD_MS+100);
  assert.deepEqual(runtime.calls.at(-1),['settled',7,'b',undefined],'a frame re-checks the confirmation hold');
});

test('pending commands wake after the unconfirmed deadline; close cancels timers', () => {
  const runtime=fakeRuntime(),clock=fakeClock();
  const spec=createDesktopSpeculation({runtime,id:1,active:()=>true,clock});
  spec.pending({invocation:'p',command:'{}'});
  clock.advance(UNCONFIRMED_DEADLINE_MS);
  assert.equal(runtime.calls.length,1);
  clock.advance(200);
  assert.deepEqual(runtime.calls.at(-1),['settled',1,'p',undefined]);
  spec.pending({invocation:'q',command:'{}'});
  spec.close();
  assert.equal(clock.size(),0);
});

test('older runtimes and inactive mounts stay durable-only', () => {
  const calls=[];
  const spec=createDesktopSpeculation({runtime:{},id:1,active:()=>true,clock:fakeClock()});
  assert.equal(spec.available,false);
  spec.report({desktopCommand:{invocation:'a',state:'succeeded',terminal:true}});
  const runtime=fakeRuntime();let live=true;
  const inactive=createDesktopSpeculation({runtime,id:2,active:()=>live,clock:fakeClock()});
  live=false;
  inactive.report({desktopCommand:{invocation:'a',state:'failed',terminal:true}});
  assert.deepEqual(runtime.calls,[['enable',2,true]]);
  assert.deepEqual(calls,[]);
});

test('a receipt binds an optimistic open to its committed window', () => {
  const runtime=fakeRuntime(),clock=fakeClock();
  const spec=createDesktopSpeculation({runtime,id:3,active:()=>true,clock});
  const opened=[{appId:'so.tilo.notes',recordId:'tab:1',strip:'18446744073709551615',group:'2',tab:'3'}];
  spec.update({invocation:'parent',state:'succeeded',terminal:true,update:{state:'succeeded',output:{value:{kind:'desktop.command',command:'dev:op',generation:4,opened}}}});
  assert.deepEqual(runtime.calls.at(-1),['settled',3,'parent',true,{opened}],'u64 ids stay decimal strings');
  const count=runtime.calls.length;
  spec.update({invocation:'parent',state:'succeeded',terminal:true,update:{state:'succeeded',output:{value:{kind:'desktop.command',opened:[]}}}});
  spec.update({invocation:'parent',state:'failed',terminal:true,update:{state:'failed',error:{code:'desktop_command_rejected',details:{reason:'not_permitted'}}}});
  spec.update({invocation:'parent',state:'succeeded',terminal:true,update:{state:'succeeded',output:{value:{kind:'other',opened}}}});
  assert.equal(runtime.calls.length,count,'native opened:[] and refusals carry no window (refusals settle through report)');
});

test('a committed notice for a v2 operation binds its Rust parent', () => {
  const runtime=fakeRuntime(),clock=fakeClock();
  const spec=createDesktopSpeculation({runtime,id:4,active:()=>true,clock});
  spec.report({desktopCommand:{invocation:'parent',operation:'operation',state:'submitting',terminal:false}});
  const opened={strip:'1',group:'2',tab:'3'};
  spec.committed({invocation:'operation',room:'room',opened});
  assert.deepEqual(runtime.calls.at(-1),['settled',4,'parent',undefined,{opened}]);
  spec.committed({invocation:'v1-invocation',room:'room',opened});
  assert.deepEqual(runtime.calls.at(-1),['settled',4,'v1-invocation',undefined,{opened}],'v1 invocations are the parent');
  spec.committed({invocation:'x',room:'room'});
  assert.equal(runtime.calls.length,3);
  spec.close();
  spec.committed({invocation:'operation',room:'room',opened});
  assert.equal(runtime.calls.length,3,'closed glue is inert');
});
