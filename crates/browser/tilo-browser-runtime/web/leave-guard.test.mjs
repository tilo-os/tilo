import assert from 'node:assert/strict';
import test from 'node:test';
import {createLeaveGuard, unfinishedWork} from './leave-guard.mjs';
import {APP_WINDOW_QUERY, followDisplayMode} from './app-window.mjs';

test('leaving asks only while work would be lost or held back', () => {
  assert.equal(unfinishedWork({}), null);
  assert.equal(unfinishedWork({jobs: ['Uploading plan.pdf']}), 'Uploading plan.pdf');
  assert.equal(unfinishedWork({pendingWrites: 2}), 'Saving your changes');
  assert.equal(unfinishedWork({outgoingPending: 1}), 'Sending your changes to the workspace');
});

function fakeWindow() {
  const listeners = new Map();
  return {listeners, addEventListener(type, fn) { listeners.set(type, fn); }, removeEventListener(type, fn) { if (listeners.get(type) === fn) listeners.delete(type); }};
}

test('the beforeunload listener stands only while there is work, and asks then', () => {
  const window = fakeWindow();
  const guard = createLeaveGuard({window});
  guard.update({jobs: [], pendingWrites: 0});
  assert.equal(window.listeners.has('beforeunload'), false, 'never nags with nothing in flight');
  guard.update({jobs: ['Uploading plan.pdf']});
  assert.equal(guard.guarding, true);
  const event = {prevented: false, preventDefault() { this.prevented = true; }};
  window.listeners.get('beforeunload')(event);
  assert.equal(event.prevented, true, 'closing mid-upload asks first');
  guard.update({outgoingPending: 3});
  guard.update({jobs: []});
  assert.equal(guard.guarding, true, 'an edit the host has not received still holds it');
  guard.update({outgoingPending: 0});
  assert.equal(window.listeners.has('beforeunload'), false, 'gone once everything landed');
});

test('the runtime hears whether the page is an app window, and when that changes', () => {
  const told = [];
  const listeners = new Set();
  const query = {matches: false, addEventListener: (_type, fn) => listeners.add(fn), removeEventListener: (_type, fn) => listeners.delete(fn)};
  const asked = [];
  const window = {matchMedia: text => { asked.push(text); return query; }};
  const stop = followDisplayMode({runtime: {set_app_window: value => told.push(value)}, window});
  assert.deepEqual(asked, [APP_WINDOW_QUERY]);
  query.matches = true;
  for (const listener of listeners) listener();
  assert.deepEqual(told, [false, true]);
  stop();
  assert.equal(listeners.size, 0);
});
