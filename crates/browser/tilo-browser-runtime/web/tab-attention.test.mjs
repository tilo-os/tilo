import assert from 'node:assert/strict';
import test from 'node:test';
import {DECLINED_KEY, badgeText, createTabAttention, shouldOffer, tabTitle} from './tab-attention.mjs';

test('the title leads with what needs the person, then the workspace', () => {
  assert.equal(tabTitle({workspace: 'Garden'}), 'Tilo · Garden');
  assert.equal(tabTitle({}), 'Tilo');
  assert.equal(tabTitle({workspace: 'Garden', unread: 3}), '(3) Tilo · Garden');
  assert.equal(tabTitle({workspace: 'Garden', unread: 250}), '(99+) Tilo · Garden');
  assert.equal(tabTitle({workspace: 'Garden', waiting: ['Claude Code']}), 'Claude Code needs you · Tilo · Garden');
  assert.equal(tabTitle({workspace: 'Garden', waiting: ['Claude Code', 'Codex']}), 'Claude Code and Codex need you · Tilo · Garden');
  assert.equal(tabTitle({workspace: 'Garden', unread: 1, finished: ['Claude Code']}), '(1) Claude Code finished · Tilo · Garden');
  assert.equal(tabTitle({workspace: 'Garden', waiting: ['Codex'], finished: ['Claude Code', 'Codex']}),
    'Codex needs you · Claude Code finished · Tilo · Garden', 'an agent waiting again is not also finished');
});

test('the icon is badged only while something needs the person', () => {
  assert.equal(badgeText({}), null);
  assert.equal(badgeText({unread: 4}), '4');
  assert.equal(badgeText({unread: 12}), '9+');
  assert.equal(badgeText({waiting: ['Claude Code']}), '!');
  assert.equal(badgeText({finished: ['Claude Code']}), '!');
});

test('notifications are offered once there is news, never at load, and never after Not now', () => {
  const news = {unread: 0, waiting: ['Claude Code'], finished: []};
  assert.equal(shouldOffer({permission: 'default', declined: false, attention: {unread: 0, waiting: [], finished: []}}), false,
    'nothing to have been told of yet');
  assert.equal(shouldOffer({permission: 'default', declined: false, attention: news}), true);
  assert.equal(shouldOffer({permission: 'default', declined: true, attention: news}), false);
  assert.equal(shouldOffer({permission: 'granted', declined: false, attention: news}), false);
  assert.equal(shouldOffer({permission: 'denied', declined: false, attention: news}), false);
  assert.equal(shouldOffer({permission: 'unavailable', declined: false, attention: news}), false);
});

function fakePage({visibility = 'visible', permission = 'default'} = {}) {
  const listeners = new Map();
  const body = {children: [], append(node) { this.children.push(node); node.parent = this; }};
  const element = () => {
    const node = {dataset: {}, style: {}, children: [], listeners: {},
      setAttribute() {}, append(...nodes) { this.children.push(...nodes); },
      addEventListener(type, fn) { this.listeners[type] = fn; },
      remove() { body.children = body.children.filter(child => child !== node); }};
    return node;
  };
  const document = {title: 'Tilo', visibilityState: visibility, body, createElement: element,
    querySelector: () => null, addEventListener: (type, fn) => listeners.set(type, fn)};
  const asked = [];
  const stored = new Map();
  const window = {document, Notification: Object.assign(function Notification() {}, {permission, requestPermission: async () => { asked.push(true); return 'granted'; }}),
    localStorage: {getItem: key => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value)}};
  return {window, document, asked, stored, show: state => { document.visibilityState = state; listeners.get('visibilitychange')?.(); }};
}

test('an agent that finished while the person was away stays in the title until they look', () => {
  const page = fakePage({visibility: 'hidden', permission: 'granted'});
  const tab = createTabAttention({window: page.window});
  tab.setWorkspace('Garden');
  tab.update({unread: 1, waiting: [], finished: [{id: 7, agent: 'Claude Code'}]});
  assert.equal(page.document.title, '(1) Claude Code finished · Tilo · Garden');
  page.show('visible');
  assert.equal(page.document.title, '(1) Tilo · Garden', 'seen on the desk once the tab is in view');
  tab.update({unread: 1, waiting: [], finished: [{id: 7, agent: 'Claude Code'}]});
  assert.equal(page.document.title, '(1) Tilo · Garden');
});

test('the offer appears with the first news in view, and Not now is kept', async () => {
  const page = fakePage();
  const tab = createTabAttention({window: page.window});
  tab.update({unread: 0, waiting: [], finished: []});
  assert.equal(page.document.body.children.length, 0, 'never at load');
  tab.update({unread: 0, waiting: ['Claude Code'], finished: []});
  const [card] = page.document.body.children;
  assert.ok(card, 'offered once there is news');
  const [, row] = card.children;
  const [notNow] = row.children;
  notNow.listeners.click();
  assert.equal(page.stored.get(DECLINED_KEY), '1');
  assert.equal(page.document.body.children.length, 0);
  const again = fakePage();
  again.stored.set(DECLINED_KEY, '1');
  again.window.localStorage = page.window.localStorage;
  createTabAttention({window: again.window}).update({unread: 2, waiting: [], finished: []});
  assert.equal(again.document.body.children.length, 0, 'a later page does not ask again');
  assert.deepEqual(page.asked, []);
});

test('Turn on asks the browser inside the click', () => {
  const page = fakePage();
  createTabAttention({window: page.window}).update({unread: 1, waiting: [], finished: []});
  const [card] = page.document.body.children;
  const [, turnOn] = card.children[1].children;
  turnOn.listeners.click();
  assert.deepEqual(page.asked, [true]);
});
