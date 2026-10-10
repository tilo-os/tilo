// Executes the production private-world observer; this is not a Blink fixture.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../src/host.cc', import.meta.url), 'utf8');
const marker = 'composition_scripts_->Configure(R"JS(';
const start = source.indexOf(marker);
assert.ok(start >= 0, 'production composition observer must be observable');
assert.equal(source.indexOf(marker, start + marker.length), -1);
const end = source.indexOf(')JS");', start);
assert.ok(end > start);
const script = source.slice(start + marker.length, end);
const page = readFileSync(new URL('../src/page_automation.js', import.meta.url), 'utf8');
const inputStart = page.indexOf('  let textLease =');
const snapshotStart = page.indexOf('  function activeTextElement()');
const snapshotEnd = page.indexOf('  function validateNativeReplacement(');
assert.ok(inputStart >= 0 && snapshotStart > inputStart && snapshotEnd > snapshotStart,
  'production field consumer must be observable');
const inputSetup = page.slice(inputStart, snapshotStart);
const inputSnapshot = page.slice(snapshotStart, snapshotEnd);
const type = page.split('\n').find(line => line.startsWith('  const type ='));
const secret = page.split('\n').find(line => line.startsWith('  const secret ='));
assert.ok(type && secret);

function fixture() {
  class Node {
    connected = true;
    get isConnected() { return this.connected; }
    set isConnected(value) { this.connected = value; }
  }
  const root = new Node();
  const document = {documentElement: root, hasFocus: () => true};
  const handlers = new Map();
  let mutation;
  const context = {
    Node, document, location: {protocol: 'https:'},
    MutationObserver: class {
      constructor(callback) { mutation = callback; }
      observe() {}
      disconnect() {}
    },
    addEventListener(kind, callback) {
      handlers.set(kind, [...(handlers.get(kind) || []), callback]);
    },
    removeEventListener(kind, callback) {
      handlers.set(kind, (handlers.get(kind) || []).filter(owned => owned !== callback));
    },
  };
  const realm = vm.createContext(context);
  vm.runInContext(script, realm, {timeout: 1000});
  const event = (kind, target, isTrusted = false) => {
    for (const callback of handlers.get(kind) || []) callback({target, isTrusted});
  };
  event('DOMContentLoaded', document, true);
  return {node: () => new Node(), event, active: () => context.__tiloTextComposition.active(),
    replaceRoot() { document.documentElement = new Node(); mutation([{removedNodes: [root]}]); },
    consumer() {
      // Ordinary fields only: no native-caret, origin or CSS qualification.
      context.ShadowRoot = class { getSelection() { return null; } };
      context.getComputedStyle = () => ({webkitTextSecurity: 'none'});
      context.failure = code => ({code});
      vm.runInContext(type + '\n' + secret + '\n' + inputSetup + inputSnapshot +
        '\nglobalThis.reviewSnapshot = textSnapshot;', realm, {timeout: 1000});
      return {
        field() {
          const field = new Node();
          Object.assign(field, {nodeType: 1, localName: 'input', type: 'text', value: 'public text',
            selectionStart: 0, selectionEnd: 6, maxLength: -1, selectionDirection: 'forward',
            getAttribute(name) { return name === 'type' ? this.type : null; }, matches() { return false; }});
          return field;
        },
        snapshot(field) { document.activeElement = field; return context.reviewSnapshot(field); },
        loseObserver() {
          // Contextified host-object deletion does not reliably remove the
          // actual realm's global on Node 22. Match the Blink probe's stimulus.
          assert.equal(vm.runInContext('delete globalThis.__tiloTextComposition', realm), true);
          assert.equal(vm.runInContext('typeof globalThis.__tiloTextComposition', realm), 'undefined');
        },
      };
    }};
}

test('an unrelated custom-editor end cannot clear the original composition', () => {
  const f = fixture(), original = f.node(), other = f.node();
  f.event('compositionstart', original);
  f.event('compositionend', other);
  assert.equal(f.active(), true);
  f.event('compositionend', original);
  assert.equal(f.active(), false);
});

test('overlapping custom-editor compositions end independently', () => {
  const f = fixture(), first = f.node(), second = f.node();
  f.event('compositionstart', first);
  f.event('compositionstart', second);
  f.event('compositionend', first);
  assert.equal(f.active(), true);
  f.event('compositionend', second);
  assert.equal(f.active(), false);
});

test('a removed custom editor no longer blocks a fresh editable successor', () => {
  const f = fixture(), old = f.node(), successor = f.node();
  f.event('compositionstart', old);
  old.isConnected = false;
  assert.equal(f.active(), false);
  f.event('compositionstart', successor);
  f.event('compositionend', old);
  assert.equal(f.active(), true);
  f.event('compositionend', successor);
  assert.equal(f.active(), false);
});

test('removal and forged ends never certify trusted native composition as idle', () => {
  const f = fixture(), native = f.node();
  f.event('compositionstart', native, true);
  native.isConnected = false;
  f.event('compositionend', native);
  assert.equal(f.active(), true);
  f.event('compositionend', native, true);
  assert.equal(f.active(), false);
});

test('document replacement remains unknown after all composition ends', () => {
  const f = fixture(), node = f.node();
  f.event('compositionstart', node);
  f.replaceRoot();
  f.event('compositionend', node);
  assert.equal(f.active(), true);
});

test('unobservable composition targets and excess targets fail closed', () => {
  for (const overflow of [false, true]) {
    const f = fixture(), nodes = Array.from({length: 65}, f.node);
    if (overflow) for (const node of nodes) f.event('compositionstart', node);
    else f.event('compositionstart', {});
    for (const node of nodes) f.event('compositionend', node);
    f.event('compositionend', {});
    assert.equal(f.active(), true);
  }
});

test('the field consumer admits only the fresh successor after custom-editor removal', () => {
  const f = fixture(), consumer = f.consumer(), original = consumer.field(), successor = consumer.field();
  assert.equal(consumer.snapshot(original).value, 'public text');
  f.event('compositionstart', original);
  assert.throws(() => consumer.snapshot(original), error => error.code === 'text_input_composition');
  original.isConnected = false;
  assert.equal(consumer.snapshot(successor).value, 'public text');
  assert.throws(() => consumer.snapshot(original), error => error.code === 'text_input_changed');
});

test('the field consumer still requires the original document-start observer', () => {
  const f = fixture(), consumer = f.consumer(), field = consumer.field();
  consumer.loseObserver();
  assert.throws(() => consumer.snapshot(field), error => error.code === 'text_input_composition');
});

test('a fresh consumer remains blocked by trusted composition and protected fields', () => {
  const f = fixture(), consumer = f.consumer(), original = consumer.field(), successor = consumer.field();
  f.event('compositionstart', original, true);
  original.isConnected = false;
  f.event('compositionend', original);
  assert.throws(() => consumer.snapshot(successor), error => error.code === 'text_input_composition');
  f.event('compositionend', original, true);
  successor.type = 'password';
  assert.throws(() => consumer.snapshot(successor), error => error.code === 'secret_field');
});
