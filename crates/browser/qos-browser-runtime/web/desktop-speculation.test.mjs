import test from 'node:test';
import assert from 'node:assert/strict';
import {createDesktopSpeculation,CONFIRMED_HOLD_MS,UNCONFIRMED_DEADLINE_MS} from './desktop-speculation.mjs';

function fakeClock(){let now=0,next=1;const timers=new Map();return {
  setTimeout:(fn,ms)=>{const id=next++;timers.set(id,{fn,at:now+ms});return id;},clearTimeout:id=>timers.delete(id),
  advance(ms){now+=ms;for(const [id,t] of [...timers].sort((a,b)=>a[1].at-b[1].at))if(t.at<=now){timers.delete(id);t.fn();}},size:()=>timers.size};}
function fakeRuntime(){const calls=[];return {calls,desktop_speculation_enable:(id,on)=>calls.push(['enable',id,on]),desktop_command_settled:(id,inv,outcome)=>{calls.push(['settled',id,inv,outcome]);return true;}};}

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
