import test from 'node:test';
import assert from 'node:assert/strict';
import {reloaded} from './waits.mjs';
import {agentCommand} from './shell.mjs';
import {setupPrompt, signInPrompt} from '../../../../crates/browser/tilo-browser-runtime/web/first-screen.mjs';

test('the page drivers run the ticket displayed in the setup and sign-in prompts', () => {
  const ticket = `tilo1${'ab'.repeat(80)}`;
  assert.deepEqual(agentCommand(setupPrompt(ticket)),
    {kind: 'login', text: ticket, ticket, code: null, site: null});
  assert.deepEqual(agentCommand(signInPrompt(ticket)),
    {kind: 'device add', text: ticket, ticket, code: null, site: null});
  assert.equal(agentCommand(signInPrompt(ticket + 'x')), null);
  assert.deepEqual(agentCommand('tilo login D9B-X4GK --site https://node.example, then follow the instructions.'),
    {kind: 'login', text: 'D9B-X4GK', ticket: null, code: 'D9B-X4GK', site: 'https://node.example'});
});

/** A tab reloading in `turns` reads: the document it loads answers as arrived
 * once its page is drawn (`data-arrival`, a page element), never by its own
 * phase. */
function reloading({turns = 2, presents = 1} = {}) {
  let turn = 0;
  let reloadedAt = null;
  return {
    tab: {
      reload: async () => {},
      documents: () => (reloadedAt === null ? 1 : 2),
      evaluate: async expression => {
        if (!expression.includes('readyState')) return true; // marking the old document
        turn += 1;
        if (reloadedAt === null && turn > turns) reloadedAt = turn;
        const old = reloadedAt === null;
        const up = presents === null ? null : reloadedAt !== null && turn > reloadedAt + presents;
        return JSON.stringify({old, ready: true, page: old ? true : Boolean(up)});
      },
    },
    turns: () => turn,
  };
}

test('a reload wait ends on the new document’s shell, never the one going away', async () => {
  // The old document keeps answering as a presented shell after the browser
  // took the reload. A wait that read that would judge the page that is going
  // away; this one ends only once the new document's shell says it is up.
  const {tab, turns} = reloading({turns: 3, presents: 1});
  let pauses = 0;
  const how = await reloaded(tab, {pause: async () => { pauses += 1; }, timeoutMs: 20_000});
  assert.equal(how, 'presented', 'the wait ended on the new document, not the old one’s shell');
  assert.equal(turns(), 6, 'it read until the new document said its shell was up');
  assert.equal(pauses, 5, 'it waited for a change between reads');
});

test('a page that presents and is replaced by the next navigation is waited past', async () => {
  // The reloaded document presents, but a navigation starts the next one while
  // it is read: the wait must not take the first for the page that stayed.
  let turn = 0;
  let documents = 1;
  const tab = {
    reload: async () => {},
    documents: () => documents,
    evaluate: async expression => {
      if (!expression.includes('readyState')) return true; // marking the old document
      turn += 1;
      if (turn === 2) documents = 2;
      if (turn === 3) documents = 3; // replaced while it was read
      const at = turn <= 1 ? {old: true, ready: true, page: true}
        : turn === 4 ? {old: false, ready: true, page: false}
        : {old: false, ready: true, page: true};
      return JSON.stringify(at);
    },
  };
  const how = await reloaded(tab, {pause: async () => {}, timeoutMs: 20_000});
  assert.equal(how, 'presented');
  assert.equal(turn, 5, 'it waited past the page that was replaced');
});

test('a reloaded document whose shell never says it is up is stalled', async () => {
  // A failure screen: the new document is there, its shell never is.
  const {tab} = reloading({turns: 1, presents: null});
  let clock = 0;
  const how = await reloaded(tab, {pause: async ms => { clock += ms; }, now: () => clock, timeoutMs: 1_000});
  assert.equal(how, 'stalled');
});

test('a document that never reloads times out instead of judging the old page', async () => {
  const tab = {reload: async () => {}, documents: () => 1, evaluate: async expression =>
    (expression.includes('readyState')
      ? JSON.stringify({old: true, ready: true, page: true})
      : true)};
  let clock = 0;
  const how = await reloaded(tab, {pause: async ms => { clock += ms; }, now: () => clock, timeoutMs: 1_000});
  assert.equal(how, 'timeout');
});
