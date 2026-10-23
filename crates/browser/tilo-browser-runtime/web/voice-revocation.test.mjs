import test from 'node:test';
import assert from 'node:assert/strict';
import {openCarrier, MOQ_PROTOCOL, STATUS_ADMITTED, STATUS_DENIED, STATUS_REVOKED} from './moq-carrier.mjs';
import {createVoiceTransport} from './voice-transport.mjs';
import {createVoiceCall, GRANT_DEADLINE_MS} from './voice.mjs';

const CALL = 'c'.repeat(64), SELF = 'a'.repeat(64);
const drain = async () => {for (let i = 0; i < 50; i++) await Promise.resolve();};

async function connected({status = STATUS_ADMITTED, expectedPhase = 'live', leaveError} = {}) {
  let admission, webTransport;
  class WebTransport {
    constructor() {
      webTransport = this;
      this.protocol = MOQ_PROTOCOL;
      this.ready = Promise.resolve();
      this.closed = new Promise(resolve => {this.disconnected = resolve;});
    }
    createBidirectionalStream() {
      return {readable:new ReadableStream({start(controller) {
        admission = controller; controller.enqueue(Uint8Array.of(status));
      }}), writable:new WritableStream({write(bytes) {assert.equal(bytes[0], 1);}})};
    }
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
  // `timers` are carrier retries; the join's grant deadline is not one.
  const sent = [], timers = [], deadlines = [], audio = {started:0, stopped:0, muted:false};
  const call = createVoiceCall({transport,
    signalling:{join:id=>sent.push(['join',id]), leave:()=>{sent.push(['leave']);if(leaveError)throw leaveError;},
      onEvent:listener=>{onEvent=listener; return ()=>{};}},
    audio:{start:async ()=>(audio.started++, {get captureActive() {return !audio.muted && !audio.stopped;},
      setMuted:async muted=>{audio.muted=muted;}, stop:async ()=>{audio.stopped++;}})},
    environment:new EventTarget(), clock:{setTimeout:(callback,delay)=>{const timer={callback,delay};(delay===GRANT_DEADLINE_MS?deadlines:timers).push(timer);return timer;}, clearTimeout:timer=>{timer.cancelled=true;}}});
  call.join(CALL);
  await drain();
  const grant = {call:CALL, participation:SELF, token:'01',
    endpoint:'https://localhost/moq/voice', publication:`${CALL}/${SELF}`, directions:{publish:true,subscribe:true},
    participants:[{namespace:`${CALL}/${SELF}`,device:SELF,user:SELF,publishes:true}]};
  onEvent({kind:'granted',grant});
  await drain();
  assert.equal(call.state.phase,expectedPhase);
  assert.equal(call.stats().captureActive,expectedPhase==='live');
  return {call,audio,timers,deadlines,sent,admission,webTransport,onEvent,grant};
}

/** The grant deadline an admission the room took away waits on, run. */
function deadlinePasses(f) {
  const waiting = f.deadlines.filter(timer=>!timer.cancelled);
  assert.equal(waiting.length,1,'the call waits for a fresh grant');
  waiting[0].callback();
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
  assert.equal(f.deadlines.filter(timer=>!timer.cancelled).length,0);
  assert.deepEqual(f.sent,[['join',CALL]],'nothing left the call');
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
  f.onEvent({kind:'granted',grant:{...f.grant,token:'02'}});
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
      this.protocol = MOQ_PROTOCOL;
      this.ready = Promise.resolve();
      this.closed = new Promise(() => {});
    }
    createBidirectionalStream() {
      return {readable:new ReadableStream({start(controller) {
        admission = controller; controller.enqueue(Uint8Array.of(STATUS_ADMITTED));
      }}), writable:new WritableStream()};
    }
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
