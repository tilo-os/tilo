import test from 'node:test';
import assert from 'node:assert/strict';
import {openCarrier, MOQ_PROTOCOL, STATUS_ADMITTED, STATUS_DENIED, STATUS_REVOKED} from './moq-carrier.mjs';
import {createVoiceTransport} from './voice-transport.mjs';
import {createVoiceCall, GRANT_DEADLINE_MS, GRANT_LOOK_MS} from './voice.mjs';

const CALL = 'c'.repeat(64), SELF = 'a'.repeat(64);
const drain = async () => {for (let i = 0; i < 50; i++) await Promise.resolve();};

async function connected({status = STATUS_ADMITTED, expectedPhase = 'live', leaveError} = {}) {
  let admission, webTransport;
  class WebTransport {
    constructor() {
      webTransport = this;
      this.negotiated = MOQ_PROTOCOL;
      this.ready = Promise.resolve();
      this.closed = new Promise(resolve => {this.disconnected = resolve;});
    }
    createBidirectionalStream() {
      return {readable:new ReadableStream({start(controller) {
        admission = controller; controller.enqueue(Uint8Array.of(status));
      }}), writable:new WritableStream({write(bytes) {assert.equal(bytes[0], 1);}})};
    }
    get protocol() {return this.negotiated;}
    close() {this.disconnected();}
    hostClose(closeCode) {this.disconnected({closeCode});}
  }
  class Producer {
    consume() {return {};}
    createBroadcast() {
      return {createTrack:()=>({close() {}}), announce() {}, close() {}};
    }
    announced() {return {next:()=>new Promise(()=>{}), close() {}};}
    close() {}
  }
  const library = {Origin:{Producer}, Time:{Timescale:{MICRO:1_000_000}},
    Path:{from:parts=>parts, Pattern:{subtree:parts=>parts}},
    Connection:{connect:async ({transport})=>({version:MOQ_PROTOCOL,
      transport:'webtransport', closed:transport.closed, close:()=>transport.close()})}};
  const transport = createVoiceTransport({library,
    carrier:options=>openCarrier({...options, WebTransport})});
  let onEvent;
  // `timers` are carrier retries; the grant deadline's looks are not.
  const sent = [], timers = [], deadlines = [], audio = {started:0, stopped:0, muted:false};
  const clock = {at:0, now:()=>clock.at, setTimeout:(callback,delay)=>{const timer={callback,delay};(delay===GRANT_LOOK_MS?deadlines:timers).push(timer);return timer;}, clearTimeout:timer=>{timer.cancelled=true;}};
  const call = createVoiceCall({transport,
    signalling:{join:id=>sent.push(['join',id]), leave:()=>{sent.push(['leave']);if(leaveError)throw leaveError;},
      onEvent:listener=>{onEvent=listener; return ()=>{};}},
    audio:{start:async ()=>(audio.started++, {get captureActive() {return !audio.muted && !audio.stopped;},
      setMuted:async muted=>{audio.muted=muted;}, stop:async ()=>{audio.stopped++;}})},
    environment:new EventTarget(), clock});
  call.join(CALL);
  await drain();
  const grant = {call:CALL, participation:SELF, token:'01',
    endpoint:'https://localhost/moq/voice', publication:`${CALL}/${SELF}`, directions:{publish:true,subscribe:true},
    participants:[{namespace:`${CALL}/${SELF}`,device:SELF,user:SELF,publishes:true}]};
  onEvent({kind:'granted',grant});
  await drain();
  assert.equal(call.state.phase,expectedPhase);
  assert.equal(call.stats().captureActive,expectedPhase==='live');
  return {call,audio,timers,deadlines,sent,admission,webTransport,onEvent,grant,clock};
}

/** The grant deadline's next look, `ms` after the last one: false when
 * none is waiting (the call got its grant, or the deadline passed). */
function look(f, ms) {
  const waiting = f.deadlines.filter(timer=>!timer.cancelled && !timer.ran);
  assert.ok(waiting.length <= 1,'one look at a time');
  if (!waiting.length) return false;
  f.clock.at += ms;
  waiting[0].ran = true;
  waiting[0].callback();
  return true;
}

/** The grant deadline an admission the room took away waits on, run on a
 * page that runs. */
function deadlinePasses(f) {
  assert.equal(f.deadlines.filter(timer=>!timer.cancelled && !timer.ran).length,1,'the call waits for a fresh grant');
  while (look(f, GRANT_LOOK_MS));
}

test('authenticated carrier revocation closes audio, then ends the call when no fresh grant comes', async () => {
  const f = await connected();
  f.admission.enqueue(Uint8Array.of(STATUS_REVOKED));
  await drain();
  assert.equal(f.call.stats().captureActive,false);
  assert.equal(f.audio.stopped,1,'explicit authority loss must dispose playback and capture, not retain a paused session');
  assert.equal(f.call.state.phase,'reconnecting','the session may still hold the participation');
  assert.equal(f.timers.length,0,'revocation must not schedule a carrier retry on the revoked grant');
  assert.deepEqual(f.sent,[['join',CALL]]);
  deadlinePasses(f);
  assert.equal(f.call.state.phase,'ended');
  assert.deepEqual(f.sent,[['join',CALL],['leave']]);
  f.call.dispose();
});

test('a call the room took the carrier from goes on with the fresh grant the session brings', async () => {
  const f = await connected();
  f.admission.enqueue(Uint8Array.of(STATUS_REVOKED));
  await drain();
  assert.equal(f.call.state.phase,'reconnecting');
  // The session joined afresh: a new participation and token.
  const fresh = 'b'.repeat(64);
  f.onEvent({kind:'granted',grant:{...f.grant, participation:fresh, token:'03', publication:`${CALL}/${fresh}`,
    participants:[{namespace:`${CALL}/${fresh}`,device:SELF,user:SELF,publishes:true}]}});
  await drain();
  assert.equal(f.call.state.phase,'live');
  assert.equal(f.audio.started,2,'audio starts again for the fresh admission');
  assert.equal(f.deadlines.filter(timer=>!timer.cancelled && !timer.ran).length,0);
  assert.deepEqual(f.sent,[['join',CALL]],'nothing left the call');
  f.call.dispose();
});

test('a page that stands still waiting for the fresh grant keeps the call until it comes', async () => {
  const f = await connected();
  f.admission.enqueue(Uint8Array.of(STATUS_REVOKED));
  await drain();
  assert.equal(f.call.state.phase,'reconnecting');
  // The page stood still for 30 s, then crawled (a look every 5 s, as a
  // page starved by software rendering does) for a minute: the session's
  // beats crawled with it, so the deadline is no nearer than one look per
  // look.
  assert.ok(look(f, 30_000));
  for (let i = 0; i < 12; i++) assert.ok(look(f, 5_000));
  assert.equal(f.call.state.phase,'reconnecting','frozen time does not use up the deadline');
  assert.deepEqual(f.sent,[['join',CALL]],'nothing left the call');
  const fresh = 'b'.repeat(64);
  f.onEvent({kind:'granted',grant:{...f.grant, participation:fresh, token:'04', publication:`${CALL}/${fresh}`,
    participants:[{namespace:`${CALL}/${fresh}`,device:SELF,user:SELF,publishes:true}]}});
  await drain();
  assert.equal(f.call.state.phase,'live','back once the session joined afresh');
  assert.equal(look(f, GRANT_LOOK_MS),false,'the grant ends the deadline');
  f.call.dispose();
});

test('a page that runs ends a call whose fresh grant never comes after the deadline', async () => {
  const f = await connected();
  f.admission.enqueue(Uint8Array.of(STATUS_REVOKED));
  await drain();
  let looks = 0;
  while (look(f, GRANT_LOOK_MS)) looks++;
  assert.equal(looks, Math.ceil(GRANT_DEADLINE_MS / GRANT_LOOK_MS));
  assert.equal(f.call.state.phase,'ended');
  f.call.dispose();
});

test('ordinary carrier loss pauses capture and retains consent for an admitted reconnect', async () => {
  const f = await connected();
  f.admission.close();
  await drain();
  assert.equal(f.call.stats().captureActive,false);
  assert.equal(f.audio.stopped,0);
  assert.equal(f.call.state.phase,'reconnecting');
  assert.equal(f.timers.length,1);
  f.call.dispose();
  assert.equal(f.audio.stopped,1);
});


test('authenticated revocation close code closes audio even without a delivered control byte', async () => {
  const f = await connected();
  f.webTransport.hostClose(0x7402);
  await drain();
  assert.equal(f.call.state.phase,'reconnecting');
  deadlinePasses(f);
  assert.equal(f.call.state.phase,'ended');
  assert.equal(f.audio.stopped,1);
  assert.equal(f.timers.filter(timer=>!timer.cancelled).length,0);
  f.call.dispose();
});

test('an explicit admission denial stops audio instead of retrying a refused grant', async () => {
  const f = await connected({status:STATUS_DENIED,expectedPhase:'reconnecting'});
  assert.equal(f.audio.stopped,1);
  deadlinePasses(f);
  assert.equal(f.call.state.phase,'ended');
  assert.equal(f.timers.length,0);
  f.call.dispose();
});

test('late revocation of a replaced grant cannot end the newly admitted carrier', async () => {
  const f = await connected();
  // The host forgot the participation and the session joined afresh: a new
  // participation, so a new carrier.
  const again = 'e'.repeat(64);
  f.onEvent({kind:'granted',grant:{...f.grant,participation:again,token:'02',publication:`${CALL}/${again}`}});
  await drain();
  assert.equal(f.call.state.phase,'live');
  f.admission.enqueue(Uint8Array.of(STATUS_REVOKED));
  await drain();
  assert.equal(f.call.state.phase,'live');
  assert.equal(f.call.stats().captureActive,true);
  assert.equal(f.audio.stopped,0);
  f.call.dispose();
});


test('revocation disposes audio even when signalling Leave throws', async () => {
  const f = await connected({leaveError:new Error('signalling disconnected')});
  f.admission.enqueue(Uint8Array.of(STATUS_REVOKED));
  await drain();
  deadlinePasses(f);
  assert.equal(f.call.state.phase,'ended');
  assert.equal(f.audio.stopped,1);
  assert.equal(f.timers.length,0);
  f.call.dispose();
});

test('a revoked carrier lets its holder close its tracks before the transport closes', async () => {
  let admission, closes = 0;
  const timers = [];
  class WebTransport {
    constructor() {
      this.negotiated = MOQ_PROTOCOL;
      this.ready = Promise.resolve();
      this.closed = new Promise(() => {});
    }
    createBidirectionalStream() {
      return {readable:new ReadableStream({start(controller) {
        admission = controller; controller.enqueue(Uint8Array.of(STATUS_ADMITTED));
      }}), writable:new WritableStream()};
    }
    get protocol() {return this.negotiated;}
    close() {closes++;}
  }
  const opened = await openCarrier({url:new URL('https://localhost/moq/live'), grant:Uint8Array.of(1),
    WebTransport, setTimer:callback=>timers.push(callback)});
  let openWhenRevoked;
  const holder = opened.revoked.then(() => {openWhenRevoked = closes === 0; opened.close();});
  admission.enqueue(Uint8Array.of(STATUS_REVOKED));
  await holder;
  assert.equal(openWhenRevoked, true, 'the holder closes its tracks while the session is still open');
  assert.equal(closes, 1);
  assert.equal(timers.length, 1, 'and the transport is closed for a holder that did not');
  timers[0]();
  assert.equal(closes, 2);
});
