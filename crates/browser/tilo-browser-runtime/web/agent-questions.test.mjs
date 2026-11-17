import test from 'node:test';
import assert from 'node:assert/strict';
import {answerAgentQuestions} from './agent-questions.mjs';

const ADA = 'ad'.repeat(32);

/** A session that records each root it is asked to answer as (`undefined`:
 * its own device key). */
const session = (userPublicKey = ADA) => {
  const calls = [];
  return {calls, userPublicKey, answerAgentQuestions: root => calls.push(root)};
};

test('a page holding the root of the person it is here as answers their agents over its session', () => {
  const peer = session();
  const identity = {userPublicKey: ADA, publicKey: new Uint8Array(32), sign: async () => new Uint8Array(64)};
  assert.equal(answerAgentQuestions({peer, identity}), true);
  assert.deepEqual(peer.calls, [identity]);
});

test('a page paired to a person whose root is elsewhere answers with its own device key', () => {
  // No root here, or another person's: the session signs with its own key.
  const peer = session();
  assert.equal(answerAgentQuestions({peer, identity: {userPublicKey: 'be'.repeat(32)}}), true);
  assert.equal(answerAgentQuestions({peer, identity: null}), true);
  assert.deepEqual(peer.calls, [undefined, undefined]);
});

test('without a session to ask over, nothing is started', () => {
  assert.equal(answerAgentQuestions({peer: null, identity: {userPublicKey: ADA}}), false);
  assert.equal(answerAgentQuestions({peer: {userPublicKey: ADA}, identity: {userPublicKey: ADA}}), false);
});

test('a session that refuses the device key leaves the page as it was', () => {
  const warned = console.warn;
  console.warn = () => {};
  try {
    const refusing = {userPublicKey: ADA, answerAgentQuestions: () => { throw new Error('device key'); }};
    assert.equal(answerAgentQuestions({peer: refusing, identity: {userPublicKey: ADA}}), false);
  } finally {
    console.warn = warned;
  }
});
