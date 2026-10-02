import test from 'node:test';
import assert from 'node:assert/strict';
import {createVoiceTransport} from './voice-transport.mjs';

const CALL = 'c'.repeat(64), SELF = 'a'.repeat(64), REMOTE = 'b'.repeat(64);
const drain = async () => {for (let i = 0; i < 10; i++) await Promise.resolve();};
test('leaving closes a publisher request that has not resolved a broadcast', async () => {
  let announce;
  let requested;
  const origins = [];
  class Producer {
    constructor() {origins.push(this);}
    announced() {return {next: () => new Promise(resolve => {announce = resolve;}), close() {announce?.();}};}
    request() {requested = {active: {peek: () => undefined, changed: () => new Promise(() => {})}, closed: false, close() {this.closed = true;}}; return requested;}
    close() {}
  }
  const library = {Origin: {Producer}, Path: {from: (...parts) => parts.join('/'), Pattern: {subtree: path => path}, stripPrefix: (prefix, path) => path.slice(prefix.length + 1)},
    Connection: {connect: async () => ({version: 'moq-lite-06', transport: 'webtransport', closed: new Promise(() => {}), close() {}})}};
  const transport = createVoiceTransport({library, carrier: async () => ({transport: {}, revoked: new Promise(() => {}), close() {}})});
  const session = await transport.connect({grant: {call: CALL, participation: SELF, token: '01', endpoint: 'https://localhost/moq/voice', directions: {publish: false, subscribe: true}}});
  session.subscribe({onFrame() {}});
  announce({kind: 'announced', prefix: `${CALL}/${REMOTE}`});
  await drain();
  assert.ok(requested, 'the actual carrier requested the announced remote');
  session.close();
  await drain();
  assert.equal(requested.closed, true, 'leave releases a pending request without waiting for the broadcaster');
});

test('a renewed publisher announcement replaces its old subscription and sequence window', async () => {
  const {encodeVoiceObject, encodeVoiceBundle} = await import('./voice-profile.mjs');
  let announce;
  const requests = [];
  class Producer {
    announced() {return {next: () => new Promise(resolve => {announce = resolve;}), close() {announce?.();}};}
    request() {
      let datagram, group;
      const track = {
        recvDatagram: () => new Promise(resolve => {datagram = resolve;}),
        recvGroup: () => new Promise(resolve => {group = resolve;}),
        close() {datagram?.(); group?.();},
      };
      const request = {active: {peek: () => ({track: () => ({subscribe: () => track})})},
        closed: false, close() {this.closed = true;},
        deliver() {datagram({payload: encodeVoiceBundle([encodeVoiceObject({sequence: 0n, timestamp: 0n, duration: 960, payload: new Uint8Array([0xf8, 0xff, 0xfe])})])});}};
      requests.push(request); return request;
    }
    close() {}
  }
  const library = {Origin: {Producer}, Path: {from: (...parts) => parts.join('/'), Pattern: {subtree: path => path}, stripPrefix: (prefix, path) => path.slice(prefix.length + 1)},
    Connection: {connect: async () => ({version: 'moq-lite-06', transport: 'webtransport', closed: new Promise(() => {}), close() {}})}};
  const transport = createVoiceTransport({library, carrier: async () => ({transport: {}, revoked: new Promise(() => {}), close() {}})});
  const session = await transport.connect({grant: {call: CALL, participation: SELF, token: '01', endpoint: 'https://localhost/moq/voice', directions: {publish: false, subscribe: true}}});
  const frames = [];
  session.subscribe({onFrame: frame => frames.push(frame)});
  announce({kind: 'announced', prefix: `${CALL}/${REMOTE}`});
  await drain();
  requests[0].deliver();
  await drain();
  assert.equal(frames.length, 1);
  // The replacement can be announced before either old receive loop ends.
  announce({kind: 'announced', prefix: `${CALL}/${REMOTE}`});
  await drain();
  assert.equal(requests.length, 2, 'renewal must request the new broadcast');
  assert.equal(requests[0].closed, true, 'the replaced request is released');
  requests[1].deliver();
  await drain();
  assert.equal(frames.length, 2, 'sequence zero belongs to the new carrier and is audible');
  session.close();
  await drain();
  assert.equal(requests[1].closed, true, 'leave releases the renewed request');
});
