// Runs the production composition observer and the text-input snapshot that
// consumes it against a minimal DOM stand-in.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const observer = readFileSync(new URL('../src/text_composition.js', import.meta.url), 'utf8');
const page = readFileSync(new URL('../src/page_automation.js', import.meta.url), 'utf8');
const slice = (from, to) => {
  const start = page.indexOf(from), end = page.indexOf(to, start);
  assert.ok(start >= 0 && end > start, `page_automation.js must contain ${from} before ${to}`);
  return page.slice(start, end);
};
const line = prefix => {
  const found = page.split('\n').find(text => text.startsWith(prefix));
  assert.ok(found, `page_automation.js must contain ${prefix}`);
  return found;
};
const consumer = [line('  const type ='), line('  const secret ='),
  slice('  let textLease =', '  function validateNativeReplacement(')].join('\n');

class Node {
  connected = true;
  get isConnected() { return this.connected; }
}

function fixture() {
  const root = new Node();
  const document = {documentElement: root, hasFocus: () => true, activeElement: null};
  const handlers = new Map();
  let mutation;
  const context = {
    Node, document, location: {protocol: 'https:'},
    MutationObserver: class {
      constructor(callback) { mutation = callback; }
      observe() {}
      disconnect() {}
    },
    addEventListener(kind, callback) { handlers.set(kind, [...(handlers.get(kind) || []), callback]); },
    removeEventListener(kind, callback) {
      handlers.set(kind, (handlers.get(kind) || []).filter(owned => owned !== callback));
    },
    ShadowRoot: class { getSelection() { return null; } },
    getComputedStyle: () => ({webkitTextSecurity: 'none'}),
    failure: code => ({code}),
    clearTimeout() {},
  };
  vm.runInNewContext(observer, context, {timeout: 1000});
  vm.runInNewContext(consumer + '\nglobalThis.textSnapshot = textSnapshot;', context, {timeout: 1000});
  const event = (kind, target, isTrusted = false) => {
    for (const callback of handlers.get(kind) || []) callback({target, isTrusted});
  };
  event('DOMContentLoaded', document, true);
  const field = () => Object.assign(new Node(), {
    nodeType: 1, localName: 'input', type: 'text', value: 'text', selectionStart: 0, selectionEnd: 4,
    maxLength: -1, selectionDirection: 'forward', readOnly: false,
    getAttribute(name) { return name === 'type' ? this.type : null; }, matches() { return false; },
  });
  return {
    event, field,
    active: () => context.__tiloTextComposition.active(),
    snapshot(element) { document.activeElement = element; return context.textSnapshot(element); },
    replaceRoot() { document.documentElement = new Node(); mutation([{removedNodes: [root]}]); },
  };
}

test('an end on another editor does not end the original composition', () => {
  const f = fixture(), original = f.field(), other = f.field();
  f.event('compositionstart', original);
  f.event('compositionend', other);
  assert.equal(f.active(), true);
  f.event('compositionend', original);
  assert.equal(f.active(), false);
});

test('overlapping editor compositions end independently', () => {
  const f = fixture(), first = f.field(), second = f.field();
  f.event('compositionstart', first);
  f.event('compositionstart', second);
  f.event('compositionend', first);
  assert.equal(f.active(), true);
  f.event('compositionend', second);
  assert.equal(f.active(), false);
});

test('a removed editor does not leave composition active', () => {
  const f = fixture(), old = f.field(), successor = f.field();
  f.event('compositionstart', old);
  old.connected = false;
  assert.equal(f.active(), false);
  f.event('compositionstart', successor);
  f.event('compositionend', old);
  assert.equal(f.active(), true);
  f.event('compositionend', successor);
  assert.equal(f.active(), false);
});

test('text input reaches a successor after the composing editor is removed', () => {
  const f = fixture(), old = f.field(), successor = f.field();
  assert.equal(f.snapshot(old).value, 'text');
  f.event('compositionstart', old);
  assert.throws(() => f.snapshot(old), error => error.code === 'text_input_composition');
  old.connected = false;
  assert.equal(f.snapshot(successor).value, 'text');
  assert.throws(() => f.snapshot(old), error => error.code === 'text_input_changed');
});

test('page events never end trusted native composition', () => {
  const f = fixture(), native = f.field(), successor = f.field();
  f.event('compositionstart', native, true);
  native.connected = false;
  f.event('compositionend', native);
  assert.equal(f.active(), true);
  assert.throws(() => f.snapshot(successor), error => error.code === 'text_input_composition');
  f.event('compositionend', native, true);
  assert.equal(f.active(), false);
});

test('a replaced document stays unknown', () => {
  const f = fixture(), node = f.field();
  f.event('compositionstart', node);
  f.replaceRoot();
  f.event('compositionend', node);
  assert.equal(f.active(), true);
});

test('more concurrent editors than tracked stay active', () => {
  const crowded = fixture(), nodes = Array.from({length: 65}, crowded.field);
  for (const node of nodes) crowded.event('compositionstart', node);
  for (const node of nodes) crowded.event('compositionend', node);
  assert.equal(crowded.active(), true);
});
