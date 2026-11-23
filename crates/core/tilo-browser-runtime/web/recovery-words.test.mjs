// The 24 recovery words card: each word in the position it is written in, the
// way to copy them, and the confirmation that lets the person go on. The
// words themselves are the runtime's (`tilo-browser-runtime`'s `Root`),
// checked in a browser by the entry's flow.
import test from 'node:test';
import assert from 'node:assert/strict';
import {RECOVERY_WORD_COUNT, RECOVERY_WORDS, recoveryCardCopy, showRecoveryWords} from './recovery-words.mjs';

/** As much of the DOM as the card uses. */
class CardNode {
  constructor(tag) {
    this.tag = tag;
    this.children = [];
    this.dataset = {};
    this.attrs = new Map();
    this.listeners = new Map();
    this.className = '';
    this.textContent = '';
    this.id = '';
    this.disabled = false;
    this.checked = false;
    this.removed = false;
  }
  append(...nodes) { this.children.push(...nodes); }
  setAttribute(name, value) { this.attrs.set(name, value); }
  addEventListener(type, listener) { this.listeners.set(type, listener); }
  remove() { this.removed = true; }
  focus() { this.focused = true; }
  fire(type, event = {preventDefault() {}}) { return this.listeners.get(type)?.(event); }
}

const page = () => {
  const body = new CardNode('body');
  return {body, document: {body, createElement: tag => new CardNode(tag)}};
};
const walk = (node, found = []) => {
  for (const child of node.children ?? []) { found.push(child); walk(child, found); }
  return found;
};
const byClass = (node, name) => walk(node).filter(child => child.className.split(' ').includes(name));
const tick = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };

/** A real 24-word phrase (BIP39's all-zero seed), as the runtime hands one
 * over: the card shows whatever it is given. */
const PHRASE = 'abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon '
  + 'abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art';

test('the card shows the 24 words in order, with their positions, and goes on once the person says they kept them', async () => {
  const {body, document} = page();
  let resolved = false;
  const shown = showRecoveryWords(PHRASE, {document, messages: new Map(), clipboard: {writeText: async () => {}}});
  shown.then(() => { resolved = true; });

  const card = body.children.find(child => child.dataset.card === 'recovery-words');
  assert.ok(card, 'the card is over the page');
  assert.equal(card.attrs.get('role'), 'dialog');
  assert.equal(card.attrs.get('aria-labelledby'), 'tilo-recovery-heading');

  const words = byClass(card, 'tilo-recovery-word');
  assert.equal(words.length, RECOVERY_WORD_COUNT);
  assert.deepEqual(words.map(word => word.dataset.position), [...Array(RECOVERY_WORD_COUNT)].map((_, at) => String(at + 1)));
  assert.deepEqual(words.map(word => word.children.find(child => child.className === 'tilo-recovery-text').textContent),
    PHRASE.split(' '), 'the words in the order they restore');

  const [go] = byClass(card, 'tilo-entry-primary');
  assert.equal(go.textContent, RECOVERY_WORDS.confirm, 'the button names what the person did');
  await tick();
  assert.equal(resolved, false, 'nothing goes on before they say they kept them');

  card.fire('submit');
  assert.equal(card.removed, true, 'the words leave the screen once confirmed');
  await tick();
  assert.equal(resolved, true, 'the page goes on');
});

test('copying hands over the words in order, and the card says it was copied', async () => {
  const {body, document} = page();
  const written = [];
  showRecoveryWords(PHRASE, {document, messages: new Map(), clipboard: {writeText: async text => { written.push(text); }}});
  const card = body.children.find(child => child.dataset.card === 'recovery-words');
  const [hand] = byClass(card, 'tilo-recovery-copy');
  assert.equal(hand.textContent, RECOVERY_WORDS.copy);
  hand.fire('click');
  await tick();
  assert.deepEqual(written, [PHRASE.split(' ').join(' ')]);
  assert.equal(hand.textContent, RECOVERY_WORDS.copied);
});

test('a phrase that is not 24 words is the page\'s fault, and says so', async () => {
  const {document} = page();
  await assert.rejects(showRecoveryWords('abandon abandon abandon', {document, messages: new Map()}), RangeError);
});

test('the card speaks in the locale it is given, and falls back to its English', () => {
  const german = recoveryCardCopy(new Map([['recovery-words-heading', 'Deine 24 Wiederherstellungswörter']]));
  assert.equal(german.heading, 'Deine 24 Wiederherstellungswörter');
  assert.equal(german.body, RECOVERY_WORDS.body, 'a key the locale lacks keeps its English');
  assert.deepEqual(recoveryCardCopy(null), {...RECOVERY_WORDS});
});

