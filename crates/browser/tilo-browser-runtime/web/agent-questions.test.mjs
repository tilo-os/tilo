import test from 'node:test';
import assert from 'node:assert/strict';
import {answerAgentQuestions} from './agent-questions.mjs';

const ADA = 'ad'.repeat(32);
const address = {endpointAddr: {id: 'ef'.repeat(32), addrs: []}, relayUrl: 'https://relay.tilo.test/'};

const runtime = () => {
  const calls = [];
  return {calls, answerAgentQuestions: (at, root) => calls.push([JSON.parse(at), root])};
};

test('a page holding the root of the person it is here as answers their agents at its host', () => {
  const answering = runtime();
  const identity = {userPublicKey: ADA, publicKey: new Uint8Array(32), sign: async () => new Uint8Array(64)};
  assert.equal(answerAgentQuestions({peer: {userPublicKey: ADA, host: {address}}, identity, runtime: answering}), true);
  assert.deepEqual(answering.calls, [[address, identity]]);
});

test('a page paired to a person whose root is elsewhere, or holding none, is no approver', () => {
  const answering = runtime();
  const peer = {userPublicKey: ADA, host: {address}};
  assert.equal(answerAgentQuestions({peer, identity: {userPublicKey: 'be'.repeat(32)}, runtime: answering}), false);
  assert.equal(answerAgentQuestions({peer, identity: null, runtime: answering}), false);
  assert.equal(answerAgentQuestions({peer: {host: {address}}, identity: {userPublicKey: ADA}, runtime: answering}), false);
  assert.deepEqual(answering.calls, []);
});

test('without a host to reach, or a runtime that answers, nothing is started', () => {
  const identity = {userPublicKey: ADA};
  const answering = runtime();
  assert.equal(answerAgentQuestions({peer: {userPublicKey: ADA}, identity, runtime: answering}), false);
  assert.equal(answerAgentQuestions({peer: {userPublicKey: ADA, host: {address}}, identity, runtime: {}}), false);
  assert.deepEqual(answering.calls, []);
});

test('a runtime that refuses the custody leaves the page as it was', () => {
  const warned = console.warn;
  console.warn = () => {};
  try {
    const refusing = {answerAgentQuestions: () => { throw new Error('custody'); }};
    assert.equal(answerAgentQuestions({peer: {userPublicKey: ADA, host: {address}}, identity: {userPublicKey: ADA}, runtime: refusing}), false);
  } finally {
    console.warn = warned;
  }
});
