import test from 'node:test';
import assert from 'node:assert/strict';
import {createMediaSession} from './media-session.mjs';

function deferred(){
 let resolve,reject;
 const promise=new Promise((ok,no)=>{resolve=ok;reject=no;});
 return {promise,resolve,reject};
}

function fakeClock(){
 let now=0,nextId=0;
 const timers=new Map();
 const flush=async()=>{for(let index=0;index<12;index++)await Promise.resolve();};
 return {
  now:()=>now,
  setTimeout(fn,delay){const id=++nextId;timers.set(id,{at:now+delay,fn});return id;},
  clearTimeout(id){timers.delete(id);},
  async tick(ms){
   const end=now+ms;
   while(true){
    const due=[...timers].filter(([,timer])=>timer.at<=end).sort((a,b)=>a[1].at-b[1].at||a[0]-b[0])[0];
    if(!due)break;
    const [id,timer]=due;timers.delete(id);now=timer.at;timer.fn();await flush();
   }
   now=end;await flush();
  },
  timers,
 };
}

function fixture(overrides={}){
 const calls=[],frames=[],clock=overrides.clock??fakeClock();
 const audioResource={
  setMuted(value){calls.push(['audio.mute',value]);},
  selectInput(value){calls.push(['audio.input',value]);},
  play(frame){frames.push(frame);},
  removePublisher(value){calls.push(['audio.removePublisher',value]);},
  stop(){calls.push(['audio.stop']);},
  ...overrides.audioResource,
 };
 const audio={
  async start(options){calls.push(['audio.start',options]);return audioResource;},
  ...overrides.audio,
 };
 let connected=0;
 const sessions=[];
 const transport={
  async connect(options){
   calls.push(['transport.connect',options]);connected++;
   const closed=deferred();
   const session={
    publish(frame){calls.push(['publish',frame]);},
    subscribe(options){calls.push(['subscribe',options]);this.subscription=options;},
    refreshAuthorization(options){calls.push(['refreshAuthorization',options]);},
    closed:closed.promise,
    close(){calls.push(['transport.close',connected]);},
    closeTransport:closed,
    ...overrides.session,
   };
   sessions.push(session);return session;
  },
  ...overrides.transport,
 };
 let grant=0;
 const authority={
  async capabilities(){calls.push(['capabilities']);return {mediaV1:{version:1}};},
  async join(request){calls.push(['authority.join',request]);return {secret:`grant-${++grant}`,expiresAt:clock.now()+60000,presenceExpiresAt:clock.now()+15000,directions:{publish:true,subscribe:true}};},
  async renew(request){calls.push(['authority.renew',request]);return {secret:`grant-${++grant}`,expiresAt:clock.now()+60000,presenceExpiresAt:clock.now()+15000,directions:{publish:true,subscribe:true}};},
  async heartbeat(request){calls.push(['authority.heartbeat',request]);return {expiresAt:clock.now()+15000};},
  async leave(request){calls.push(['authority.leave',request]);},
  ...overrides.authority,
 };
 const errors=[];
 let authorizationControl;
 const controller=createMediaSession({authority,transport,audio,clock,onError:error=>errors.push(error),bindAuthorizationControl:control=>{authorizationControl=control;}});
 return {controller,authority,transport,audio,audioResource,sessions,calls,frames,errors,clock,expire:()=>authorizationControl?.expire()};
}

const publicSnapshot=(state='idle',muted=true,callId=null,reason=null)=>({state,muted,callId,reason});

test('exports a small versioned capability and reload starts idle and muted',()=>{
 const first=fixture(),second=fixture();
 assert.equal(first.controller.version,1);
 assert.deepEqual(Object.keys(first.controller).sort(),['dispose','getSnapshot','join','leave','selectInput','setMuted','subscribe','version']);
 assert.deepEqual(first.controller.getSnapshot(),publicSnapshot());
 assert.deepEqual(second.controller.getSnapshot(),publicSnapshot());
 first.controller.dispose();second.controller.dispose();
});

test('cancelled join stops late audio and leaves no authority or transport resource',async()=>{
 const pending=deferred();
 let stopped=0;
 const f=fixture({audio:{start:()=>pending.promise}});
 const joining=f.controller.join({callId:'call-a'});
 await Promise.resolve();
 const leaving=f.controller.leave();
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('ended',true,null,'left'));
 pending.resolve({setMuted(){},selectInput(){},play(){},stop(){stopped++;}});
 await joining;await leaving;
 assert.equal(stopped,1);
 assert.equal(f.calls.some(([name])=>name==='authority.join'),false);
 assert.equal(f.calls.some(([name])=>name==='transport.connect'),false);
 assert.equal(f.clock.timers.size,0);
});

test('join wires publication and playback without exposing the private grant',async()=>{
 const f=fixture();
 const snapshots=[];f.controller.subscribe(snapshot=>snapshots.push(snapshot));
 await f.controller.join({callId:'voice'});
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('live',true,'voice'));
 assert(snapshots.some(snapshot=>snapshot.state==='preparing'));
 assert(snapshots.some(snapshot=>snapshot.state==='joining'));
 await f.controller.setMuted(false);
 const started=f.calls.find(([name])=>name==='audio.start')[1];
 started.onFrame({payload:new Uint8Array([7]),timestamp:42,samples:960});
 assert.equal(f.calls.filter(([name])=>name==='publish').length,1);
 f.sessions[0].subscription.onFrame({payload:new Uint8Array([8]),timestamp:43,samples:960});
 assert.equal(f.frames.length,1);
 const serialized=JSON.stringify({snapshot:f.controller.getSnapshot(),snapshots});
 assert.equal(serialized.includes('grant-'),false);
 await f.controller.leave();
});

test('renewal failure ends participation and releases every resource',async()=>{
 const f=fixture({authority:{async renew(request){f.calls.push(['authority.renew',request]);throw Object.assign(Error('expired'),{code:'expired'});}}});
 await f.controller.join({callId:'voice'});
 await f.clock.tick(20000);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('ended',true,null,'renewal-expired'));
 assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,1);
 assert.equal(f.calls.filter(([name])=>name==='transport.close').length,1);
 assert.equal(f.calls.filter(([name])=>name==='authority.leave').length,1);
 assert.equal(f.clock.timers.size,0);
});

test('permission errors are bounded to unavailable media state and allocate no remote resources',async()=>{
 const denied=Object.assign(Error('browser denied microphone'),{name:'NotAllowedError'});
 const f=fixture({audio:{async start(){throw denied;}}});
 await assert.rejects(f.controller.join({callId:'voice'}),error=>error===denied);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('unavailable',true,null,'permission'));
 assert.equal(f.calls.some(([name])=>name==='authority.join'),false);
 assert.equal(f.calls.some(([name])=>name==='transport.connect'),false);
 assert.equal(f.clock.timers.size,0);
 assert.deepEqual(f.errors,[denied]);
});

test('capability rejection is bounded and does not ask for capture permission',async()=>{
 const broken=Object.freeze(Error('native authority unavailable'));
 const f=fixture({authority:{async capabilities(){throw broken;}}});
 await assert.rejects(f.controller.join({callId:'voice'}),error=>error===broken);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('unavailable',true,null,'capability'));
 assert.equal(f.calls.some(([name])=>name==='audio.start'),false);
 assert.deepEqual(f.errors,[broken]);
});

test('mute intent is immediate, controls real capture, and a new participation resets muted',async()=>{
 const f=fixture();await f.controller.join({callId:'voice'});
 await f.controller.setMuted(false);
 assert.equal(f.controller.getSnapshot().muted,false);
 f.sessions[0].subscription.onFrame({payload:new Uint8Array()});
 const start=f.calls.find(([name])=>name==='audio.start')[1];
 start.onFrame({payload:new Uint8Array()});
 assert.deepEqual(f.calls.filter(([name])=>name==='audio.mute').at(-1),['audio.mute',false]);
 await f.controller.leave();
 await f.controller.join({callId:'another'});
 assert.equal(f.controller.getSnapshot().muted,true,'explicit join starts a fresh muted participation');
 assert.equal(f.calls.filter(([name])=>name==='audio.start').at(-1)[1].muted,true);
 await f.controller.leave();
});

test('callback failures are reported, isolated, and disposal synchronously releases resources',async()=>{
 const callbackError=Error('subscriber failed');
 let callbackCalls=0;
 const f=fixture();
 f.controller.subscribe(()=>{callbackCalls++;if(callbackCalls===2)throw callbackError;});
 await f.controller.join({callId:'voice'});
 assert(f.errors.includes(callbackError));
 const disposing=f.controller.dispose();
 assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,1);
 assert.equal(f.calls.filter(([name])=>name==='transport.close').length,1);
 await disposing;
 assert.equal(f.clock.timers.size,0);
 await assert.rejects(f.controller.join({callId:'later'}),/disposed/);
});

test('late transport result after leave closes itself and cannot resurrect the call',async()=>{
 const pending=deferred(),started=deferred();let lateCloses=0;
 const f=fixture({transport:{connect:()=>{started.resolve();return pending.promise;}}});
 const joining=f.controller.join({callId:'voice'});
 await started.promise;
 const leaving=f.controller.leave();
 pending.resolve({publish(){},subscribe(){},close(){lateCloses++;}});
 await joining;await leaving;
 assert.equal(lateCloses,1);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('ended',true,null,'left'));
 assert.equal(f.clock.timers.size,0);
});

test('recovery uses bounded timers, never busy-loops, and stops after sixty seconds',async()=>{
 const clock=fakeClock(),calls=[];let connects=0,subscription;
 const transport={async connect(options){
  calls.push(['connect',clock.now(),options]);connects++;
  if(connects>1)throw Error('offline');
  return {publish(){throw Error('connection lost');},subscribe(value){subscription=value;},refreshAuthorization(){},closed:new Promise(()=>{}),close(){calls.push(['close',clock.now()]);}};
 }};
 const f=fixture({clock,transport});
 await f.controller.join({callId:'voice'});await f.controller.setMuted(false);
 f.calls.find(([name])=>name==='audio.start')[1].onFrame({payload:new Uint8Array()});
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('reconnecting',false,'voice','connection'));
 assert.equal(Math.min(...[...clock.timers.values()].map(timer=>timer.at)),250);
 await clock.tick(60000);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('ended',false,null,'recovery-timeout'));
 assert(connects>=10&&connects<=20,`bounded recovery made ${connects} attempts`);
 assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,1);
 assert.equal(f.calls.filter(([name])=>name==='authority.leave').length,1);
 assert.equal(clock.timers.size,0);
 assert(subscription);
});

test('a grant arriving after cancellation is immediately released',async()=>{
 const pending=deferred(),started=deferred(),calls=[];
 const f=fixture({authority:{
  async capabilities(){return {mediaV1:{version:1}};},
  join(){started.resolve();return pending.promise;},
  async leave(request){calls.push(request);},
 }});
 const joining=f.controller.join({callId:'voice'});
 await started.promise;
 const leaving=f.controller.leave();
 pending.resolve({secret:'late-grant'});
 await joining;await leaving;
 assert.equal(calls.length,1);
 assert.equal(calls[0].grant.secret,'late-grant');
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('ended',true,null,'left'));
});

test('renewal is not accepted until the connected carrier refreshes authorization',async()=>{
 const f=fixture();await f.controller.join({callId:'voice'});
 await f.clock.tick(20000);
 const refresh=f.calls.filter(([name])=>name==='refreshAuthorization');
 assert.equal(refresh.length,1);
 assert.equal(refresh[0][1].grant.secret,'grant-2');
 assert.equal(refresh[0][1].signal instanceof AbortSignal,true);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('live',true,'voice'));
 await f.controller.leave();
});

test('presence heartbeat runs every five seconds with participation context',async()=>{
 const f=fixture();await f.controller.join({callId:'voice'});
 await f.clock.tick(10000);
 const heartbeats=f.calls.filter(([name])=>name==='authority.heartbeat');
 assert.equal(heartbeats.length,2);
 for(const [,context] of heartbeats){
  assert.equal(context.callId,'voice');assert.equal(typeof context.participationId,'string');assert.equal(context.generation,1);
 }
 await f.controller.leave();
 assert.equal(f.clock.timers.size,0);
});

test('closed carrier triggers recovery while muted and reuses the current mute intent',async()=>{
 const f=fixture();await f.controller.join({callId:'voice'});
 f.sessions[0].closeTransport.resolve();await Promise.resolve();await Promise.resolve();
 assert.equal(f.controller.getSnapshot().state,'reconnecting');
 await f.clock.tick(250);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('live',true,'voice'));
 assert.equal(f.sessions.length,2);
 assert.equal(f.calls.filter(([name])=>name==='audio.start').length,1);
 await f.controller.leave();
});

test('old carrier frames and publish rejections cannot disturb a replacement carrier',async()=>{
 const firstPublish=deferred(),secondPublish=deferred(),publishes=[firstPublish,secondPublish];let connects=0;
 const f=fixture({transport:{async connect(options){
  f.calls.push(['transport.connect',options]);const closed=deferred(),number=connects++;
  const session={closed:closed.promise,closeTransport:closed,
   publish(){return number===0?publishes.shift().promise:undefined;},
   subscribe(value){this.subscription=value;},refreshAuthorization(){},close(){f.calls.push(['transport.close',number]);}};
  f.sessions.push(session);return session;
 }}});
 await f.controller.join({callId:'voice'});await f.controller.setMuted(false);
 const capture=f.calls.find(([name])=>name==='audio.start')[1].onFrame;
 capture({payload:new Uint8Array([1])});capture({payload:new Uint8Array([2])});
 const oldFrame=f.sessions[0].subscription.onFrame;
 firstPublish.reject(Error('first publish failed'));
 for(let index=0;index<4;index++)await Promise.resolve();
 await f.clock.tick(250);assert.equal(f.controller.getSnapshot().state,'live');
 const played=f.frames.length;oldFrame({payload:new Uint8Array([3])});assert.equal(f.frames.length,played);
 const oldPublisherEnded=f.sessions[0].subscription.onPublisherEnded;
 oldPublisherEnded('publisher-old');
 assert.equal(f.calls.filter(([name])=>name==='audio.removePublisher').length,0);
 f.sessions[1].subscription.onPublisherEnded('publisher-current');
 assert.deepEqual(f.calls.filter(([name])=>name==='audio.removePublisher').at(-1),['audio.removePublisher','publisher-current']);
 secondPublish.reject(Error('late old publish failed'));
 for(let index=0;index<4;index++)await Promise.resolve();
 assert.equal(f.controller.getSnapshot().state,'live');
 await f.controller.leave();
});

test('absolute recovery deadline aborts a hung renewal and releases capture at sixty seconds',async()=>{
 const never=deferred();
 const f=fixture({session:{publish(){throw Error('offline');}},authority:{renew:()=>never.promise}});
 await f.controller.join({callId:'voice'});await f.controller.setMuted(false);
 f.calls.find(([name])=>name==='audio.start')[1].onFrame({payload:new Uint8Array()});
 await f.clock.tick(60000);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('ended',false,null,'recovery-timeout'));
 assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,1);
 assert.equal(f.clock.timers.size,0);
});

test('grant expiry independently stops capture when scheduled renewal never settles',async()=>{
 const never=deferred();
 const f=fixture({authority:{renew:()=>never.promise}});
 await f.controller.join({callId:'voice'});
 await f.clock.tick(60000);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('ended',true,null,'renewal-expired'));
 assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,1);
 assert.equal(f.clock.timers.size,0);
});

test('leave awaits local async cleanup but never waits for native leave',async()=>{
 const stopped=deferred(),closed=deferred(),nativeLeave=deferred();let stopCalls=0,closeCalls=0,settled=false;
 const f=fixture({audioResource:{stop(){stopCalls++;return stopped.promise;}},session:{close(){closeCalls++;return closed.promise;}},authority:{leave:()=>nativeLeave.promise}});
 await f.controller.join({callId:'voice'});
 const leaving=f.controller.leave().then(()=>{settled=true;});
 assert.equal(stopCalls,1);assert.equal(closeCalls,1);assert.equal(settled,false);
 stopped.resolve();closed.resolve();for(let index=0;index<16;index++)await Promise.resolve();
 assert.equal(settled,true);nativeLeave.resolve();await leaving;
});

test('mute changes made while capture starts are reconciled onto the acquired resource',async()=>{
 const start=deferred(),calls=[];
 const f=fixture({audio:{start:()=>start.promise}});
 const joining=f.controller.join({callId:'voice'});await Promise.resolve();
 await f.controller.setMuted(false);
 start.resolve({setMuted(value){calls.push(value);},selectInput(){},play(){},stop(){}});
 await joining;
 assert.deepEqual(calls,[false]);assert.equal(f.controller.getSnapshot().muted,false);
 await f.controller.leave();
});

test('unsettled mute blocks replacement and its late rejection cannot overwrite the next participation',async()=>{
 const pending=deferred();let starts=0;
 const f=fixture({audioResource:{setMuted(){if(starts++===0)return pending.promise;}}});
 await f.controller.join({callId:'first'});
 const muting=f.controller.setMuted(false).catch(()=>{});
 await f.controller.leave();
 await assert.rejects(f.controller.join({callId:'second'}),error=>error?.code==='cleanup-incomplete');
 pending.reject(Object.assign(Error('old permission'),{name:'NotAllowedError'}));await muting;await flush();
 await f.controller.join({callId:'second'});
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('live',true,'second'));
 await f.controller.leave();
});

test('unsettled input selection blocks replacement until its old operation settles',async()=>{
 const pending=deferred();let selections=0;
 const f=fixture({audioResource:{selectInput(){if(selections++===0)return pending.promise;}}});
 await f.controller.join({callId:'first'});
 const selecting=f.controller.selectInput('old-device').catch(()=>{});
 await f.controller.leave();
 await assert.rejects(f.controller.join({callId:'second'}),error=>error?.code==='cleanup-incomplete');
 pending.reject(Object.assign(Error('old device denied'),{name:'NotAllowedError'}));await selecting;await flush();
 await f.controller.join({callId:'second'});
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('live',true,'second'));
 await f.controller.leave();
});

test('dispose is idempotent and returns the same bounded local cleanup promise',async()=>{
 const stopped=deferred();
 const f=fixture({audioResource:{stop:()=>stopped.promise}});await f.controller.join({callId:'voice'});
 const first=f.controller.dispose(),second=f.controller.dispose();assert.equal(first,second);
 stopped.resolve();await first;
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('ended',true,null,'disposed'));
});

test('permission failure during recovery is terminal without another retry',async()=>{
 const denied=Object.assign(Error('revoked'),{name:'NotAllowedError'});
 const f=fixture({session:{publish(){throw Error('offline');}},authority:{async renew(){throw denied;}}});
 await f.controller.join({callId:'voice'});await f.controller.setMuted(false);
 f.calls.find(([name])=>name==='audio.start')[1].onFrame({payload:new Uint8Array()});
 await f.clock.tick(250);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('ended',false,null,'permission'));
 assert.equal(f.clock.timers.size,0);assert.equal(f.calls.filter(([name])=>name==='transport.connect').length,1);
});

test('hung recovery connect is aborted and any late carrier closes without reviving capture',async()=>{
 const pending=[],signals=[];let connects=0,lateCloses=0;
 const f=fixture({transport:{connect(options){
  connects++;if(connects===1)return {publish(){throw Error('offline');},subscribe(value){this.subscription=value;},refreshAuthorization(){},closed:new Promise(()=>{}),close(){}};
  const value=deferred();pending.push(value);signals.push(options.signal);return value.promise;
 }}});
 await f.controller.join({callId:'voice'});await f.controller.setMuted(false);
 f.calls.find(([name])=>name==='audio.start')[1].onFrame({payload:new Uint8Array()});
 await f.clock.tick(60000);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('ended',false,null,'recovery-timeout'));
 assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,1);
 assert(signals.length>0&&signals.every(signal=>signal.aborted));
 pending[0].resolve({publish(){},subscribe(){},refreshAuthorization(){},closed:new Promise(()=>{}),close(){lateCloses++;}});
 for(let index=0;index<12;index++)await Promise.resolve();
 assert.equal(lateCloses,1);
 await f.clock.tick(10000);
});

test('late grant from a timed-out recovery attempt does not leave the active participation',async()=>{
 const first=deferred();let renews=0;
 const f=fixture({session:{publish(){throw Error('offline');}},authority:{renew(){renews++;return renews===1?first.promise:{secret:`recovery-${renews}`,expiresAt:f.clock.now()+60000,presenceExpiresAt:f.clock.now()+15000,directions:{publish:true,subscribe:true}};}}});
 await f.controller.join({callId:'voice'});await f.controller.setMuted(false);
 f.calls.find(([name])=>name==='audio.start')[1].onFrame({payload:new Uint8Array()});
 await f.clock.tick(10250);
 first.resolve({secret:'late'});for(let index=0;index<12;index++)await Promise.resolve();
 assert.equal(f.calls.filter(([name])=>name==='authority.leave').length,0);
 await f.clock.tick(5000);assert.equal(f.controller.getSnapshot().state,'live');
 await f.controller.leave();
});

test('capture frames arriving after a synchronous publish fault see no null carrier exception',async()=>{
 const f=fixture({session:{publish(){throw Error('offline');}}});
 await f.controller.join({callId:'voice'});await f.controller.setMuted(false);
 const capture=f.calls.find(([name])=>name==='audio.start')[1].onFrame;
 capture({payload:new Uint8Array([1])});
 assert.doesNotThrow(()=>capture({payload:new Uint8Array([2])}));
 await f.controller.leave();
});

test('dispose after Leave preserves the same in-flight local cleanup promise',async()=>{
 const stopped=deferred();let settled=false;
 const f=fixture({audioResource:{stop:()=>stopped.promise}});await f.controller.join({callId:'voice'});
 const leaving=f.controller.leave();leaving.then(()=>{settled=true;});
 const disposing=f.controller.dispose();
 assert.equal(disposing,leaving);assert.equal(settled,false);
 stopped.resolve();await disposing;assert.equal(settled,true);
});

test('an acquired transport with an incomplete contract is still closed',async()=>{
 let closes=0;
 const f=fixture({transport:{async connect(){return {publish(){},subscribe(){},closed:new Promise(()=>{}),close(){closes++;}};}}});
 await assert.rejects(f.controller.join({callId:'voice'}),/Incomplete media transport/);
 assert.equal(closes,1);assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,1);
});

test('bounded cleanup timeout rejects and retains explicit failed custody',async()=>{
 const never=deferred();
 const f=fixture({audioResource:{stop:()=>never.promise}});await f.controller.join({callId:'voice'});
 const leaving=f.controller.leave();let outcome='pending';leaving.then(()=>outcome='resolved',()=>outcome='rejected');
 await f.clock.tick(10000);
 await assert.rejects(leaving,error=>error?.code==='cleanup-incomplete');
 assert.equal(outcome,'rejected');assert.equal(f.controller.dispose(),leaving);
 never.resolve();
});

test('verified grant deadline is not extended by controller receipt',async()=>{
 const f=fixture({authority:{renew(){return new Promise(()=>{});},async join(){return {secret:'short',expiresAt:3000,presenceExpiresAt:15000,directions:{publish:true,subscribe:true}};}}});
 await f.controller.join({callId:'voice'});
 await f.clock.tick(3000);
 assert.equal(f.controller.getSnapshot().state,'ended');
 assert.equal(f.controller.getSnapshot().reason,'renewal-expired');
 assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,1);
});

test('heartbeat honors verified presence deadline rather than restarting fifteen seconds',async()=>{
 const f=fixture({authority:{async heartbeat(){return {expiresAt:6000};}}});
 await f.controller.join({callId:'voice'});
 await f.clock.tick(6000);
 assert.equal(f.controller.getSnapshot().state,'ended');
 assert.equal(f.controller.getSnapshot().reason,'presence-expired');
});

test('already expired grant cannot open a media carrier',async()=>{
 const f=fixture({authority:{async join(){return {secret:'expired',expiresAt:0,presenceExpiresAt:15000,directions:{publish:true,subscribe:true}};}}});
 await assert.rejects(f.controller.join({callId:'voice'}));
 assert.equal(f.calls.some(([name])=>name==='transport.connect'),false);
 assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,1);
});

test('publish-only grant opens live carrier without subscribing or receiving',async()=>{
 const f=fixture({authority:{async join(){return {expiresAt:60000,presenceExpiresAt:15000,directions:{publish:true,subscribe:false}};}},session:{subscribe(){throw Error('receive is not authorized');}}});
 try{await f.controller.join({callId:'call'});assert.equal(f.controller.getSnapshot().state,'live');
  assert.equal(f.sessions[0].subscription,undefined);await f.controller.setMuted(false);
  const capture=f.calls.find(value=>value[0]==='audio.start')[1];capture.onFrame({timestamp:0,bytes:Uint8Array.of(1)});
  assert.equal(f.calls.filter(value=>value[0]==='publish').length,1);assert.equal(f.frames.length,0);
 }finally{await f.controller.dispose();}
});

const flush=async()=>{for(let index=0;index<40;index++)await Promise.resolve();};
const freshGrant=(clock,secret='fresh')=>({secret,expiresAt:clock.now()+60000,presenceExpiresAt:clock.now()+15000,directions:{publish:true,subscribe:true}});

test('common authority expiry stops capture first and coalesces recovery with fresh grant and preserved intent',async()=>{
 const stopped=deferred(),authorized=deferred();let stops=0;
 const f=fixture({audioResource:{stop(){f.calls.push(['audio.stop']);if(stops++===0)return stopped.promise;}},authority:{reauthorize(context){f.calls.push(['reauthorize',context]);return authorized.promise;}}});
 await f.controller.join({callId:'voice'});await f.controller.selectInput('mic-a');await f.controller.setMuted(false);
 const oldCapture=f.calls.find(([name])=>name==='audio.start')[1].onFrame,oldFrame=f.sessions[0].subscription.onFrame;
 const recovering=f.expire();
 assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,1,'expiry synchronously stops actual capture');
 assert.equal(f.calls.filter(([name])=>name==='transport.close').length,1);
 assert(f.calls.findIndex(([name])=>name==='audio.stop')<f.calls.findIndex(([name])=>name==='transport.close'));
 assert.equal(f.expire(),recovering,'repeated expiry shares one recovery');
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('reconnecting',false,'voice','authorization'));
 oldCapture({payload:Uint8Array.of(1)});oldFrame({payload:Uint8Array.of(2)});
 assert.equal(f.calls.filter(([name])=>name==='publish').length,0);assert.equal(f.frames.length,0);
 await flush();assert.equal(f.calls.filter(([name])=>name==='reauthorize').length,0,'wait for raw local stop settlement');
 stopped.resolve();await flush();
 const requests=f.calls.filter(([name])=>name==='reauthorize');assert.equal(requests.length,1);
 const original=f.calls.find(([name])=>name==='authority.join')[1],context=requests[0][1];
 assert.equal(context.callId,'voice');assert(context.generation>original.generation);assert.notEqual(context.participationId,original.participationId);
 assert.equal(context.grant.secret,'grant-1','old grant is only the retained lookup');
 authorized.resolve(freshGrant(f.clock));await recovering;
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('live',false,'voice'));
 const starts=f.calls.filter(([name])=>name==='audio.start');assert.equal(starts.length,2);assert.equal(starts[1][1].inputId,'mic-a');assert.equal(starts[1][1].muted,true);
 assert.deepEqual(f.calls.filter(([name])=>name==='audio.mute').at(-1),['audio.mute',false]);
 assert.equal(f.calls.filter(([name])=>name==='transport.connect').at(-1)[1].grant.secret,'fresh');
 assert.equal(f.calls.filter(([name])=>name==='authority.join').length,1,'reauthorize owns the fresh governed participation');
 assert.equal(f.calls.filter(([name])=>name==='authority.leave').length,0,'expiry must not cancel retained Rust intent');
 oldCapture({payload:Uint8Array.of(3)});oldFrame({payload:Uint8Array.of(4)});
 assert.equal(f.calls.filter(([name])=>name==='publish').length,0);assert.equal(f.frames.length,0);
 await f.controller.leave();assert.equal(f.clock.timers.size,0);
});

test('authority expiry waits for an abandoned raw renewal before fresh authorization',async()=>{
 const renewal=deferred();
 const f=fixture({authority:{renew:()=>renewal.promise,async reauthorize(context){f.calls.push(['reauthorize',context]);return freshGrant(f.clock);}}});
 await f.controller.join({callId:'voice'});await f.clock.tick(20000);
 const recovering=f.expire();await flush();
 assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,1);
 assert.equal(f.calls.filter(([name])=>name==='reauthorize').length,0);
 renewal.resolve(freshGrant(f.clock,'old-renewal'));await recovering;
 assert.equal(f.calls.filter(([name])=>name==='reauthorize').length,1);
 assert.equal(f.calls.filter(([name])=>name==='refreshAuthorization').length,0);
 assert.equal(f.calls.filter(([name])=>name==='authority.leave').length,0);
 assert.equal(f.controller.getSnapshot().state,'live');await f.controller.leave();
});

test('Leave during reauthorization cancels old intent and releases a late fresh grant without restart',async()=>{
 const authorized=deferred();
 const f=fixture({authority:{reauthorize(context){f.calls.push(['reauthorize',context]);return authorized.promise;}}});
 await f.controller.join({callId:'voice'});const recovering=f.expire();await flush();
 assert.equal(f.calls.filter(([name])=>name==='reauthorize').length,1);
 const leaving=f.controller.leave();
 assert.equal(f.calls.filter(([name])=>name==='authority.leave')[0][1].grant.secret,'grant-1');
 authorized.resolve(freshGrant(f.clock,'late-fresh'));await recovering;await leaving;await flush();
 assert.equal(f.calls.filter(([name])=>name==='authority.leave').at(-1)[1].grant.secret,'late-fresh');
 assert.equal(f.calls.filter(([name])=>name==='audio.start').length,1);
 assert.equal(f.calls.filter(([name])=>name==='transport.connect').length,1);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('ended',true,null,'left'));
});

test('mute and input changes during authorization recovery control the replacement capture',async()=>{
 const authorized=deferred();
 const f=fixture({authority:{reauthorize:()=>authorized.promise}});
 await f.controller.join({callId:'voice'});await f.controller.setMuted(false);
 const recovering=f.expire();await flush();await f.controller.setMuted(true);await f.controller.selectInput('mic-b');
 authorized.resolve(freshGrant(f.clock));await recovering;
 const starts=f.calls.filter(([name])=>name==='audio.start');assert.equal(starts.length,2);assert.equal(starts[1][1].inputId,'mic-b');
 assert.equal(f.controller.getSnapshot().muted,true);assert.deepEqual(f.calls.filter(([name])=>name==='audio.mute').at(-1),['audio.mute',true]);
 await f.controller.leave();
});

for(const code of ['permission','revoked','expired','authorization-changed'])test(`reauthorization ${code} is terminal and cannot autojoin a changed scope`,async()=>{
 const f=fixture({authority:{async reauthorize(){throw Object.assign(Error(code),{code,terminal:true});}}});
 await f.controller.join({callId:'voice'});await f.expire();
 assert.equal(f.controller.getSnapshot().state,'ended');
 assert.equal(f.calls.filter(([name])=>name==='audio.start').length,1);
 assert.equal(f.calls.filter(([name])=>name==='authority.join').length,1);
 assert.equal(f.calls.filter(([name])=>name==='transport.connect').length,1);
 assert.equal(f.clock.timers.size,0);
});

test('expiry before an established grant ends the pending Join without automatic reauthorization',async()=>{
 const joiningGrant=deferred();let started;
 const ready=new Promise(resolve=>{started=resolve;});
 const f=fixture({authority:{join(){started();return joiningGrant.promise;},reauthorize(){throw Error('must not reauthorize unknown scope');}}});
 const joining=f.controller.join({callId:'voice'});await ready;
 const expired=f.expire();assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,1);
 joiningGrant.resolve(freshGrant(f.clock,'late-initial'));await joining;await expired;
 assert.equal(f.controller.getSnapshot().state,'ended');assert.equal(f.calls.filter(([name])=>name==='transport.connect').length,0);
});

test('timed-out raw reauthorization cannot overlap a retry or install its late grant',async()=>{
 const authorized=deferred();let attempts=0;
 const f=fixture({authority:{reauthorize(){attempts++;return authorized.promise;}}});
 await f.controller.join({callId:'voice'});const recovering=f.expire();await flush();
 await f.clock.tick(15000);
 assert.equal(attempts,1,'aborted raw authorization still owns its operation');
 await assert.rejects(f.controller.join({callId:'other'}),error=>error?.code==='cleanup-incomplete');
 authorized.resolve(freshGrant(f.clock,'late-after-timeout'));await recovering;await flush();
 assert.equal(f.controller.getSnapshot().state,'ended');
 assert.equal(f.calls.filter(([name])=>name==='audio.start').length,1);
 assert.equal(f.calls.filter(([name])=>name==='authority.leave').at(-1)[1].grant.secret,'late-after-timeout');
 assert.equal(f.clock.timers.size,0);
});

test('unknown old capture cleanup blocks reauthorization and a new explicit Join',async()=>{
 const stopped=deferred();let authorizations=0;
 const f=fixture({audioResource:{stop:()=>stopped.promise},authority:{reauthorize(){authorizations++;return freshGrant(f.clock);}}});
 await f.controller.join({callId:'voice'});const recovering=f.expire();const failure=recovering.catch(error=>error);
 await f.clock.tick(10000);
 assert.equal(f.controller.getSnapshot().state,'ended');assert.equal(authorizations,0);
 await assert.rejects(f.controller.join({callId:'other'}),error=>error?.code==='cleanup-incomplete');
 await f.clock.tick(10000);assert.equal((await failure)?.code,'cleanup-incomplete');
 stopped.resolve();await flush();await f.controller.dispose().catch(()=>{});
});

test('common authority network recovery has one absolute bounded deadline',async()=>{
 let authorizations=0;
 const f=fixture({authority:{async reauthorize(){authorizations++;throw Error('offline');}}});
 await f.controller.join({callId:'voice'});const recovering=f.expire();await flush();
 await f.clock.tick(60000);await recovering;
 assert(authorizations>1&&authorizations<=20,`bounded recovery made ${authorizations} attempts`);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('ended',true,null,'authorization-expired'));
 assert.equal(f.calls.filter(([name])=>name==='audio.start').length,1);assert.equal(f.clock.timers.size,0);
});

test('a replayed or widened authorization result cannot reopen capture',async()=>{
 for(const widened of [false,true]){
  let retained;
  const f=fixture({authority:{async join(){retained={...freshGrant(f.clock,'original'),handle:1,directions:{publish:false,subscribe:true}};return retained;},async reauthorize(){return widened?{...freshGrant(f.clock),handle:2}:retained;}}});
  await f.controller.join({callId:'voice'});await f.expire();
  assert.equal(f.controller.getSnapshot().state,'ended');
  assert.equal(f.calls.filter(([name])=>name==='audio.start').length,1);
  assert.equal(f.calls.filter(([name])=>name==='transport.connect').length,1);
 }
});

test('expiry waits for raw microphone permission settlement even after audio stop returns',async()=>{
 const permission=deferred();let authorizations=0,first=true;
 const f=fixture({audioResource:{setMuted(value){f.calls.push(['audio.mute',value]);if(first&&!value){first=false;return permission.promise;}}},authority:{async reauthorize(){authorizations++;return freshGrant(f.clock);}}});
 await f.controller.join({callId:'voice'});const unmuting=f.controller.setMuted(false);
 const recovering=f.expire();await flush();
 assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,1);
 assert.equal(authorizations,0,'an outstanding raw getUserMedia may still acquire a device');
 permission.resolve();await unmuting;await recovering;
 assert.equal(authorizations,1);assert.equal(f.controller.getSnapshot().muted,false);await f.controller.leave();
});

test('a second authority expiry during fresh audio preparation fences it under the same recovery budget',async()=>{
 const secondAudio=deferred();let starts=0,authorizations=0,lateStops=0,secondSignal;
 const f=fixture({audio:{start(options){f.calls.push(['audio.start',options]);starts++;if(starts===2){secondSignal=options.signal;return secondAudio.promise;}return f.audioResource;}},authority:{async reauthorize(){return freshGrant(f.clock,`fresh-${++authorizations}`);}}});
 await f.controller.join({callId:'voice'});const recovering=f.expire();await flush();
 assert.equal(starts,2);assert.equal(secondSignal.aborted,false);
 assert.equal(f.expire(),recovering,'one original recovery promise and deadline');
 assert.equal(secondSignal.aborted,true,'fresh in-flight capture is fenced by renewed authority expiry');
 secondAudio.resolve({setMuted(){},selectInput(){},play(){},stop(){lateStops++;}});
 await recovering;
 assert.equal(lateStops,1);assert.equal(authorizations,2);assert.equal(starts,3);
 assert.equal(f.calls.filter(([name])=>name==='transport.connect').at(-1)[1].grant.secret,'fresh-2');
 assert.equal(f.controller.getSnapshot().state,'live');await f.controller.leave();
});

test('failed actual stop retains failed custody and cannot reauthorize or start another call',async()=>{
 let authorizations=0;
 const f=fixture({audioResource:{stop(){throw Error('device stop failed');}},authority:{async reauthorize(){authorizations++;return freshGrant(f.clock);}}});
 await f.controller.join({callId:'voice'});await f.expire().catch(()=>{});
 assert.equal(authorizations,0);assert.equal(f.controller.getSnapshot().state,'ended');
 await assert.rejects(f.controller.join({callId:'other'}),error=>error?.code==='cleanup-incomplete');
 assert.equal(f.calls.filter(([name])=>name==='audio.start').length,1);
});

test('Leave after fresh recovery permission request blocks replacement until the raw request settles',async()=>{
 const permission=deferred(),reached=deferred();let starts=0;
 const f=fixture({audio:{start(options){
  const id=++starts;f.calls.push(['audio.start',options]);
  return {...f.audioResource,setMuted(value){if(id===2&&!value){reached.resolve();return permission.promise;}return f.audioResource.setMuted(value);}};
 }},authority:{async join(){return {...freshGrant(f.clock),handle:1};},async reauthorize(){return {...freshGrant(f.clock),handle:2};}}});
 await f.controller.join({callId:'voice'});await f.controller.setMuted(false);
 const recovering=f.expire();await reached.promise;
 const leaving=f.controller.leave();await flush();
 assert.equal(f.controller.getSnapshot().state,'ended');
 assert.equal(f.calls.filter(([name])=>name==='audio.stop').length,2);
 await assert.rejects(f.controller.join({callId:'replacement'}),error=>error?.code==='cleanup-incomplete');
 assert.equal(starts,2,'no replacement getUserMedia while old permission is unresolved');
 permission.resolve();await flush();await recovering;await leaving;
 await f.controller.join({callId:'replacement'});
 assert.equal(starts,3);assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('live',true,'replacement'));
 await f.controller.leave();assert.equal(f.clock.timers.size,0);
});

test('fresh carrier failure with failed actual audio stop keeps replacement Join fenced',async()=>{
 let starts=0,connects=0;
 const f=fixture({audio:{start(options){
  const id=++starts;f.calls.push(['audio.start',options]);
  return {...f.audioResource,stop(){f.calls.push(['audio.stop',id]);if(id===2)throw Error('fresh actual stop failed');}};
 }},transport:{async connect(){
  if(++connects===2)throw Error('fresh carrier failed');
  return {publish(){},subscribe(){},refreshAuthorization(){},closed:new Promise(()=>{}),close(){}};
 }},authority:{async join(){return {...freshGrant(f.clock),handle:1};},async reauthorize(){return {...freshGrant(f.clock),handle:2};}}});
 await f.controller.join({callId:'voice'});await f.controller.setMuted(false);
 await assert.rejects(f.expire(),error=>error?.code==='cleanup-incomplete');
 assert.equal(f.controller.getSnapshot().state,'ended');assert.equal(starts,2);
 assert.equal(f.controller.getSnapshot().reason,'cleanup-incomplete');
 await assert.rejects(f.controller.join({callId:'replacement'}),error=>error?.code==='cleanup-incomplete');
 assert.equal(starts,2);assert.equal(connects,2);
 assert(f.errors.some(error=>error.message==='fresh actual stop failed'));
});

test('a superseded participation rejoins the same call in place and keeps microphone intent', async()=>{
 let renews=0;
 const f=fixture({authority:{async renew(request){f.calls.push(['authority.renew',request]);renews++;throw Object.assign(Error('Media participation superseded'),{code:'stale'});}}});
 const states=[];f.controller.subscribe(value=>states.push(value.state));
 await f.controller.join({callId:'call-1'});
 await f.controller.setMuted(false);
 const firstJoin=f.calls.filter(([name])=>name==='authority.join').length;
 await f.clock.tick(20000);
 for(let index=0;index<5;index++)await f.clock.tick(10);
 assert.equal(renews,1);
 assert.equal(f.calls.filter(([name])=>name==='authority.join').length,firstJoin+1,'fresh participation joined');
 const joins=f.calls.filter(([name])=>name==='authority.join').map(([,request])=>request);
 assert.notEqual(joins[1].participationId,joins[0].participationId);
 assert.deepEqual(f.controller.getSnapshot(),publicSnapshot('live',false,'call-1',null));
 assert.ok(!states.includes('ended'),'an in-place rejoin never publishes an ended call');
 assert.ok(states.includes('reconnecting'));
 assert.ok(f.calls.some(([name])=>name==='authority.leave'),'the superseded participation is released');
});

test('leave during an in-place rejoin ends the call and never rejoins', async()=>{
 const gate=deferred();let joins=0;
 const f=fixture({authority:{
  async join(request){f.calls.push(['authority.join',request]);if(++joins===2)await gate.promise;return {secret:`grant-${joins}`,expiresAt:f.clock.now()+60000,presenceExpiresAt:f.clock.now()+15000,directions:{publish:true,subscribe:true}};},
  async renew(){throw Object.assign(Error('superseded'),{code:'stale'});},
 }});
 await f.controller.join({callId:'call-1'});
 await f.clock.tick(20010);
 assert.equal(f.controller.getSnapshot().state,'reconnecting');
 await f.controller.leave();
 gate.resolve();await f.clock.tick(10);
 assert.equal(f.controller.getSnapshot().state,'ended');
 assert.equal(f.controller.getSnapshot().reason,'left');
});

test('an in-place rejoin retries while the durable session re-authenticates, then goes live', async()=>{
 let joins=0;
 const f=fixture({authority:{
  async join(request){f.calls.push(['authority.join',request]);joins++;if(joins===2||joins===3)throw Object.assign(Error('Media control unavailable'),{code:'unavailable'});return {secret:`grant-${joins}`,expiresAt:f.clock.now()+60000,presenceExpiresAt:f.clock.now()+15000,directions:{publish:true,subscribe:true}};},
  async renew(){throw Object.assign(Error('superseded'),{code:'stale'});},
 }});
 const states=[];f.controller.subscribe(value=>states.push(value.state));
 await f.controller.join({callId:'call-1'});
 await f.clock.tick(20010);
 for(let index=0;index<20;index++)await f.clock.tick(100);
 assert.equal(joins,4);
 assert.equal(f.controller.getSnapshot().state,'live');
 assert.ok(!states.includes('ended'));
});
