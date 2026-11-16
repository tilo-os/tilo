import test from 'node:test';
import assert from 'node:assert/strict';
import {ALLOWED, notifyThisBrowser} from './web-push.mjs';

/** A browser with a push manager, and a session whose host pushes with `key`. */
function browser({permission = 'granted', key = [4, 1, 2]} = {}) {
  const kept = [];
  const subscribed = [];
  const listeners = {};
  const subscription = {
    endpoint: 'https://push.example/send/abc', options: {applicationServerKey: new Uint8Array(key).buffer},
    getKey: name => new Uint8Array(name === 'p256dh' ? [4, 9] : [7, 7]).buffer, unsubscribe: async () => true,
  };
  const pushManager = {getSubscription: async () => null, subscribe: async options => { subscribed.push(options); return subscription; }};
  const window = {
    Notification: {permission}, PushManager: function PushManager() {},
    navigator: {serviceWorker: {register: async () => ({pushManager}), ready: Promise.resolve()}},
    addEventListener: (name, listener) => { listeners[name] = listener; },
  };
  const peer = {pushKey: async () => new Uint8Array(key), notifyAt: async (...args) => { kept.push(args); }};
  return {window, peer, kept, subscribed, listeners};
}

test('a browser the person lets notify them is subscribed with the host, as this page', async () => {
  const {window, peer, kept, subscribed} = browser();
  assert.equal(await notifyThisBrowser({peer, window}), true);
  assert.deepEqual([...subscribed[0].applicationServerKey], [4, 1, 2]);
  assert.equal(subscribed[0].userVisibleOnly, true);
  assert.deepEqual(kept, [[undefined, 'https://push.example/send/abc', new Uint8Array([4, 9]), new Uint8Array([7, 7])]]);
});

test('nothing is subscribed until the person allows notifications, then it is', async () => {
  const {window, peer, kept, listeners} = browser({permission: 'default'});
  assert.equal(await notifyThisBrowser({peer, window}), false);
  assert.deepEqual(kept, []);
  window.Notification.permission = 'granted';
  listeners[ALLOWED]();
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(kept.length, 1);
});

test('a host that pushes nothing is not subscribed to', async () => {
  const {window, peer, kept} = browser({key: []});
  assert.equal(await notifyThisBrowser({peer, window}), false);
  assert.deepEqual(kept, []);
});
