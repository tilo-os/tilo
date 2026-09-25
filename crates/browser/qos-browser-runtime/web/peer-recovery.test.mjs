import test from 'node:test';
import assert from 'node:assert/strict';
import {createRecoveryCoordinator} from './peer-recovery.mjs';
function clock(){let time=0,id=0;const timers=new Map();return{now:()=>time,random:()=>0,setTimeout(fn,delay){timers.set(++id,{fn,at:time+delay});return id;},clearTimeout:id=>timers.delete(id),async tick(ms){time+=ms;for(const [id,t]of [...timers])if(t.at<=time){timers.delete(id);t.fn();}for(let i=0;i<12;i++)await Promise.resolve();},timers};}
test('automatic recovery retries within one second, caps delay, and survives failed attempts',async()=>{
 const c=clock();let calls=0,available=false;const states=[];
 const r=createRecoveryCoordinator({attempt:async()=>{calls++;if(!available)throw Error('offline');},...c,onState:s=>states.push(s)});
 r.schedule();assert.equal([...c.timers.values()][0].at,500);await c.tick(500);assert.equal(calls,1);
 // Ladder at random()=0 (half the nominal delay): 1s, 2s, 4s, then the 5s cap.
 await c.tick(1000);await c.tick(2000);await c.tick(4000);assert.equal(calls,4);assert(states.every(s=>s.nextAttemptAtMs===null||s.nextAttemptAtMs-c.now()<=5000));
 available=true;await c.tick(2500);assert.equal(calls,5);assert.equal(c.timers.size,0);r.dispose();
});
test('a session that never settles keeps backing off; one that holds earns a fast retry',async()=>{
 const c=clock();const delays=[];
 const r=createRecoveryCoordinator({attempt:async()=>{},...c,onState:s=>{if(s.nextAttemptAtMs!==null)delays.push(s.nextAttemptAtMs-c.now());}});
 // Reaching ready then dropping again immediately must not restart the ladder.
 for(let index=0;index<3;index++){r.reset();await c.tick(10);r.schedule();await c.tick(20000);}
 assert.deepEqual(delays.slice(0,3),[1000,2000,2500],'unstable sessions escalate to the 5 s cap');
 // A session that stays ready past the window settles the fault.
 r.reset();await c.tick(30000);r.schedule();
 assert.equal(delays.at(-1),500,'a session that held resets the ladder');
 r.dispose();
});
test('manual and online triggers coalesce; disposal ignores stale completion',async()=>{
 const c=clock();let calls=0,finish;const states=[];
 const r=createRecoveryCoordinator({attempt:()=>{calls++;return new Promise(resolve=>finish=resolve);},...c,onState:s=>states.push(s)});
 r.schedule();const a=r.retry(),b=r.retry();assert.equal(a,b);assert.equal(c.timers.size,0);await Promise.resolve();assert.equal(calls,1);
 r.dispose();const count=states.length;finish();await a;assert.equal(states.length,count);await c.tick(9000);assert.equal(calls,1);
});
test('terminal authentication failure stops automatic retry; corrected manual retry works',async()=>{
 const c=clock();let denied=true,calls=0;const r=createRecoveryCoordinator({attempt:async()=>{calls++;if(denied)throw Object.assign(Error('revoked'),{terminal:true});},...c});
 await assert.rejects(r.retry(),/revoked/);assert.equal(c.timers.size,0);denied=false;await r.retry();assert.equal(calls,2);r.dispose();
});
test('publication failure during a transport attempt schedules another attempt after it settles',async()=>{
 const c=clock();let calls=0,r;r=createRecoveryCoordinator({attempt:async()=>{calls++;if(calls===1)r.schedule();},...c});
 await r.retry();assert.equal(c.timers.size,1);await c.tick(500);assert.equal(calls,2);r.dispose();
});
test('fractional jitter publishes integer EpochMs deadlines matching capped coalesced timers',async()=>{
 const c=clock(),epoch=1789720459000,states=[];
 const r=createRecoveryCoordinator({...c,now:()=>epoch+c.now(),random:()=>0.314285,attempt:async()=>{throw Error('offline');},onState:s=>states.push(s)});
 r.schedule();r.schedule();
 for(let index=0;index<5;index++){
  assert.equal(c.timers.size,1,'repeated schedule requests coalesce');
  const next=[...c.timers.values()][0],state=states.at(-1),delay=next.at-c.now();
  assert(Number.isSafeInteger(delay));assert(delay>=500&&delay<=5000);
  assert(Number.isSafeInteger(state.nextAttemptAtMs));assert.equal(state.nextAttemptAtMs,epoch+next.at);
  await c.tick(delay);
 }
 r.dispose();
});
