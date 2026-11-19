import assert from 'node:assert/strict';
import test from 'node:test';
import {OFFERED_KEY, TURN_ON_ACTION, badgeText, createTabAttention, shouldOffer, tabTitle} from './tab-attention.mjs';

test('the title leads with what needs the person, then the workspace', () => {
  assert.equal(tabTitle({workspace: 'Garden'}), 'Tilo · Garden');
  assert.equal(tabTitle({}), 'Tilo');
  assert.equal(tabTitle({workspace: 'Garden', unread: 3}), '(3) Tilo · Garden');
  assert.equal(tabTitle({workspace: 'Garden', unread: 250}), '(99+) Tilo · Garden');
  assert.equal(tabTitle({workspace: 'Garden', waiting: ['Claude Code']}), 'Claude Code needs you · Tilo · Garden');
  assert.equal(tabTitle({workspace: 'Garden', waiting: ['Claude Code', 'Codex']}), 'Claude Code and Codex need you · Tilo · Garden');
});

test('the icon is badged only while something needs the person', () => {
  assert.equal(badgeText({}), null);
  assert.equal(badgeText({unread: 4}), '4');
  assert.equal(badgeText({unread: 12}), '9+');
  assert.equal(badgeText({waiting: ['Claude Code']}), '!');
});

test('notifications are offered once there is news, never at load, and never twice', () => {
  const news = {unread: 0, waiting: ['Claude Code']};
  assert.equal(shouldOffer({permission: 'default', offered: false, attention: {unread: 0, waiting: []}}), false,
    'nothing to have been told of yet');
  assert.equal(shouldOffer({permission: 'default', offered: false, attention: news}), true);
  assert.equal(shouldOffer({permission: 'default', offered: true, attention: news}), false);
  assert.equal(shouldOffer({permission: 'granted', offered: false, attention: news}), false);
  assert.equal(shouldOffer({permission: 'denied', offered: false, attention: news}), false);
  assert.equal(shouldOffer({permission: 'unavailable', offered: false, attention: news}), false);
});

function fakePage({visibility = 'visible', permission = 'default'} = {}) {
  const listeners = new Map();
  const document = {title: 'Tilo', visibilityState: visibility, querySelector: () => null,
    addEventListener: (type, fn) => listeners.set(type, fn)};
  const asked = [];
  const stored = new Map();
  const window = {document, Notification: Object.assign(function Notification() {}, {permission, requestPermission: async () => { asked.push(true); return 'granted'; }}),
    localStorage: {getItem: key => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value)}};
  const offers = [];
  const mount = {offerHostAction: offer => offers.push(offer)};
  return {window, document, asked, stored, offers, mount, show: state => { document.visibilityState = state; listeners.get('visibilitychange')?.(); }};
}

test('the offer is a notification on the shell with the first news in view, asked once', () => {
  const page = fakePage();
  const tab = createTabAttention({window: page.window});
  tab.attach(page.mount);
  tab.update({unread: 0, waiting: []});
  assert.equal(page.offers.length, 0, 'never at load');
  tab.update({unread: 0, waiting: ['Claude Code']});
  assert.equal(page.offers.length, 1, 'offered once there is news');
  const [offer] = page.offers;
  assert.equal(offer.action.id, TURN_ON_ACTION);
  assert.match(offer.title, /agent needs you/);
  tab.update({unread: 1, waiting: ['Claude Code']});
  assert.equal(page.offers.length, 1, 'one ask: put off, it waits in the notification centre');
  assert.equal(page.stored.get(OFFERED_KEY), '1');
  const again = fakePage();
  again.window.localStorage = page.window.localStorage;
  const later = createTabAttention({window: again.window});
  later.attach(again.mount);
  later.update({unread: 2, waiting: []});
  assert.equal(again.offers.length, 0, 'a later page does not ask again');
  assert.deepEqual(page.asked, []);
});

test('a hidden tab offers nothing until it is looked at', () => {
  const page = fakePage({visibility: 'hidden'});
  const tab = createTabAttention({window: page.window});
  tab.attach(page.mount);
  tab.update({unread: 1, waiting: []});
  assert.equal(page.offers.length, 0);
  page.show('visible');
  assert.equal(page.offers.length, 1);
});

test('Turn on asks the browser', async () => {
  const page = fakePage();
  const tab = createTabAttention({window: page.window});
  assert.ok(tab.handles(TURN_ON_ACTION));
  assert.ok(!tab.handles('something-else'));
  tab.perform(TURN_ON_ACTION);
  await Promise.resolve();
  assert.deepEqual(page.asked, [true]);
});
