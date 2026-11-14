import test from 'node:test';
import assert from 'node:assert/strict';
import {chooseCarryTarget, copyInto, createCarryOut, saveFromLink} from './carry-out.mjs';

// Just what the drag handling touches: capture listeners, `closest`, and
// events dispatched to the canvas.
class Node {
  constructor() { this.listeners = new Map(); this.sent = []; }
  addEventListener(type, listener, options = {}) {
    const all = this.listeners.get(type) ?? [];
    all.push(listener);
    this.listeners.set(type, all);
    options.signal?.addEventListener?.('abort', () => this.listeners.set(type, (this.listeners.get(type) ?? []).filter(l => l !== listener)));
  }
  fire(type, event) {
    const full = {type, defaultPrevented: false, preventDefault() { this.defaultPrevented = true; }, stopImmediatePropagation() {}, ...event};
    for (const listener of this.listeners.get(type) ?? []) listener(full);
    return full;
  }
  dispatchEvent(event) { this.sent.push(event); return true; }
}

class FakeEvent {
  constructor(type, init) { Object.assign(this, init, {type}); this.defaultPrevented = false; }
}

function page() {
  const root = new Node();
  const canvas = new Node();
  const doc = new Node();
  doc.activeElement = null;
  const row = {dataset: {tiloCarries: 'k1'}, closest: () => row};
  const plain = {dataset: {}, closest: () => null};
  canvas.closest = () => null;
  const view = {document: doc, PointerEvent: FakeEvent, MouseEvent: FakeEvent, WheelEvent: FakeEvent, setTimeout: fn => fn()};
  const asked = [];
  let ready = null;
  const carry = createCarryOut({root, canvas, view, link: key => { asked.push(key); return ready; }});
  return {root, canvas, doc, row, plain, asked, carry, setReady: value => { ready = value; }};
}

/** The pointer a carrying row takes still reaches the canvas, and coming
 * near the row asks for its link; a drag with no link yet is no file drag. */
test('a carrying row hands its pointer on and asks for its link early', () => {
  const p = page();
  p.root.fire('pointerover', {target: p.row, relatedTarget: null, pointerType: 'mouse', clientX: 5, clientY: 6});
  p.root.fire('pointerdown', {target: p.row, pointerType: 'mouse', clientX: 5, clientY: 6, buttons: 1});
  p.root.fire('pointerdown', {target: p.plain, pointerType: 'mouse', clientX: 1, clientY: 1});
  assert.deepEqual(p.asked, ['k1', 'k1']);
  assert.deepEqual(p.canvas.sent.map(event => event.type), ['pointerover', 'pointerdown'], 'only the carrying row\'s');
  // From the canvas onto the row and back: the surface was never left.
  p.root.fire('pointerover', {target: p.row, relatedTarget: p.canvas, pointerType: 'mouse'});
  p.root.fire('pointerout', {target: p.row, relatedTarget: p.canvas, pointerType: 'mouse'});
  const left = p.canvas.fire('pointerout', {relatedTarget: p.row, stopImmediatePropagation() { this.stopped = true; }});
  assert.equal(left.stopped, true, 'the canvas does not hear it leave for the row');
  assert.equal(p.canvas.sent.length, 2);
  // A pointer's click was the canvas's; a click with no pointer (the
  // keyboard, assistive technology) is the element's own.
  const clicked = p.root.fire('click', {target: p.row, detail: 1, stopPropagation() { this.stopped = true; }});
  assert.equal(clicked.stopped, true);
  const activated = p.root.fire('click', {target: p.row, detail: 0, stopPropagation() { this.stopped = true; }});
  assert.equal(activated.stopped, undefined);
  const data = {setData() { throw new Error('no link, no data'); }};
  const start = p.root.fire('dragstart', {target: p.row, dataTransfer: data, clientX: 9, clientY: 6});
  assert.equal(start.defaultPrevented, true, 'no file drag without a link');
  assert.equal(p.carry.dragging(), false);
});

/** A drag with a link carries `DownloadURL`; over the page it is the
 * window's own drag, moved and let go on the canvas; dropped outside, the
 * window's drag ends without landing. */
test('a drag out names the file and stays the window\'s drag over the page', () => {
  const p = page();
  p.setReady({url: 'https://host.example/peer/v1/content/aa/plan.md', mime: 'application/octet-stream', name: 'Q3: plan.md'});
  p.root.fire('pointerdown', {target: p.row, pointerType: 'mouse', pointerId: 7, clientX: 5, clientY: 6, buttons: 1});
  const data = {set: {}, setData(type, value) { this.set[type] = value; }};
  const start = p.root.fire('dragstart', {target: p.row, dataTransfer: data, clientX: 9, clientY: 6});
  assert.equal(start.defaultPrevented, false);
  assert.equal(data.effectAllowed, 'copy');
  assert.equal(data.set.DownloadURL, 'application/octet-stream:Q3_ plan.md:https://host.example/peer/v1/content/aa/plan.md');
  // The browser cancels the pointer as its drag begins: not the window's end.
  p.root.fire('pointercancel', {target: p.row, pointerType: 'mouse', pointerId: 7});
  const over = p.doc.fire('dragover', {clientX: 40, clientY: 50, dataTransfer: {}});
  assert.equal(over.defaultPrevented, true, 'the page takes the drop');
  p.doc.fire('dragover', {clientX: 40, clientY: 50, dataTransfer: {}});
  const sent = p.canvas.sent.map(event => [event.type, event.clientX, event.clientY, event.pointerId]);
  assert.deepEqual(sent, [['pointerdown', 5, 6, 7], ['pointermove', 40, 50, 7]], 'a move once per place, as the same pointer');
  p.doc.fire('drop', {clientX: 41, clientY: 52});
  p.root.fire('dragend', {target: p.row});
  assert.deepEqual(p.canvas.sent.at(-1).type, 'pointerup', 'dropped in the page: let go there');
  assert.equal(p.carry.dragging(), false);

  p.root.fire('pointerdown', {target: p.row, pointerType: 'mouse', pointerId: 8, clientX: 5, clientY: 6, buttons: 1});
  p.root.fire('dragstart', {target: p.row, dataTransfer: data, clientX: 9, clientY: 6});
  p.doc.fire('dragover', {clientX: 2, clientY: 3, dataTransfer: {}});
  p.root.fire('dragend', {target: p.row});
  const end = p.canvas.sent.at(-1);
  assert.deepEqual([end.type, end.clientX, end.clientY], ['pointercancel', 2, 3], 'dropped on the computer: the window\'s drag ends unlanded');
  p.carry.close();
  p.root.fire('pointerdown', {target: p.row, pointerType: 'mouse'});
  assert.equal(p.canvas.sent.at(-1), end, 'closed: nothing handed on');
});

/** The Save dialog where the browser has one, inside the gesture; else a
 * download; the link is streamed into the chosen file or followed. */
test('a save streams the link into the chosen file or downloads it', async () => {
  const handle = {written: [], async createWritable() { return {name: 'writable'}; }};
  const chosen = await chooseCarryTarget('plan.md', {window: {showSaveFilePicker: async ({suggestedName}) => ({...handle, suggestedName}), navigator: {userActivation: {isActive: true}}}});
  assert.equal(chosen.handle.suggestedName, 'plan.md');
  assert.equal(await chooseCarryTarget('plan.md', {window: {showSaveFilePicker: async () => { throw Object.assign(new Error('no'), {name: 'AbortError'}); }}}), null);
  assert.deepEqual(await chooseCarryTarget('plan.md', {window: {}}), {download: 'plan.md'});
  assert.deepEqual(await chooseCarryTarget('plan.md', {window: {showSaveFilePicker: () => assert.fail('no gesture, no dialog'), navigator: {userActivation: {isActive: false}}}}), {download: 'plan.md'});

  let piped;
  const body = {pipeTo: async writable => { piped = writable; }};
  await saveFromLink(chosen, 'https://host/x', {fetch: async (url, init) => {
    assert.equal(init.credentials, 'omit');
    return {ok: true, body};
  }});
  assert.deepEqual(piped, {name: 'writable'});
  await assert.rejects(saveFromLink(chosen, 'https://host/x', {fetch: async () => ({ok: false, status: 404, text: async () => 'this link has expired'})}), /expired/);

  const clicked = [];
  const doc = {body: {append() {}}, createElement: () => ({style: {}, click() { clicked.push([this.href, this.download]); }, remove() {}})};
  await saveFromLink({download: 'plan.md'}, 'https://host/x', {document: doc});
  assert.deepEqual(clicked, [['https://host/x', 'plan.md']]);
});

test('a copy reads every piece into the one buffer it started with', async () => {
  const total = 5 * 1024 * 1024 + 17;
  let sent = 0;
  const body = new ReadableStream({
    type: 'bytes',
    pull(controller) {
      const request = controller.byobRequest;
      const n = Math.min(request.view.byteLength, total - sent);
      new Uint8Array(request.view.buffer, request.view.byteOffset, n).fill(sent % 251);
      sent += n;
      request.respond(n);
      if (sent === total) controller.close();
    },
  });
  const buffers = new Set();
  let written = 0;
  const writable = new WritableStream({
    write(chunk) { buffers.add(chunk.byteLength && chunk.buffer.byteLength); written += chunk.byteLength; },
  });
  await copyInto(body, writable);
  assert.equal(written, total);
  assert.deepEqual([...buffers], [1 << 20], 'no piece came in a buffer of its own size');
});
