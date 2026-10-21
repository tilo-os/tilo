import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {chosenWorkspace, enteredWorkspaces, rememberEntered} from './workspaces.mjs';
import {AdmissionError} from './admission.mjs';

const USER = 'aa'.repeat(32);
const ORIGIN = 'https://node.example';
const tick = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };

class Element {
  constructor(tag) { this.tag = tag; this.children = []; this.listeners = new Map(); this.dataset = {}; }
  append(...nodes) { this.children.push(...nodes); }
  prepend(...nodes) { this.children.unshift(...nodes); }
  setAttribute() {}
  addEventListener(type, listener) { this.listeners.set(type, listener); }
  focus() {}
  select() {}
  remove() { document.body.children = document.body.children.filter(node => node !== this); }
}

// Run the real page wiring and workspace actions. Replace only the booted
// WASM/GPU peer, arrival animation, and root custody with fixtures.
async function fixture({boot, remembered = []} = {}) {
  const values = new Map();
  globalThis.localStorage = {getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value), removeItem: key => values.delete(key)};
  globalThis.sessionStorage = {getItem: () => null, removeItem() {}};
  globalThis.location = {href: 'https://web.example/#/room', search: '', pathname: '/', reload: () => calls.push('reload'), assign: () => calls.push('assign')};
  globalThis.history = {pushState: () => calls.push('history')};
  globalThis.document = {body: new Element('body'), documentElement: {dataset: {}}, createElement: tag => new Element(tag),
    getElementById: id => id === 'status' ? status : id === 'surface' ? new Element('canvas') : null};
  const status = {textContent: 'Loading…'};
  const calls = [];
  const shown = [];
  let diagnostics;
  const peer = {origin: ORIGIN, room: 'current', userPublicKey: USER,
    host: {hostCert: {user_public_key: Array(32).fill(170), device_public_key: Array(32).fill(0xcd)}},
    createInvitation: async () => { calls.push('invite'); return 'signed-secret'; },
    membership: {list: async () => ({name: 'Current', members: []})},
    workspaces: {create: async name => ({room: 'created', name}), enter: async () => calls.push('enter')},
    dispose: async () => calls.push('dispose')};
  const page = {peer, entry: {base: 'https://web.example/'},
    mount: {setHostActions: actions => shown.push(actions), flush: async () => calls.push('flush'), cleanup: async () => calls.push('cleanup')},
    router: {route: {target: {scope: {kind: 'room', host: ORIGIN, room: 'current'}}}, stop: () => calls.push('stop')},
    link: {format: route => `https://web.example/#/${route.target.scope.room}`}};
  const prepared = [];
  globalThis.__entryPageFixture = {page, identity: {userPublicKey: USER}, prepareGpu: () => prepared.push(document.documentElement.dataset.arrival),
    boot: async options => { diagnostics = options.diagnostics; return boot ? boot(options, page) : page; }};
  rememberEntered({origin: ORIGIN, room: 'other', hostDevice: 'cd'.repeat(32), name: 'Other'});
  // Rooms an earlier visit kept as they were stored, whichever host held them.
  if (remembered.length) {
    localStorage.setItem('tilo-entered-workspaces', JSON.stringify([...JSON.parse(localStorage.getItem('tilo-entered-workspaces')), ...remembered]));
  }
  let source = await readFile(new URL('./entry-page.mjs', import.meta.url), 'utf8');
  source = source
    .replace("import {boot, grammar} from './entry.mjs';", 'const boot = globalThis.__entryPageFixture.boot;')
    .replace("import {loadRuntime, mountDesktop, prefetchApplication, prepareGpu} from './host.mjs';", 'const loadRuntime = async () => ({}), mountDesktop = () => {}, prefetchApplication = () => {}, prepareGpu = () => globalThis.__entryPageFixture.prepareGpu();')
    .replace("import {failureMessage, startArrival, STOPPED_DRAWING} from './arrival.mjs';", "const startArrival = () => {}, failureMessage = error => error.message, STOPPED_DRAWING = 'stopped';")
    .replace("import {watchForUpdate} from './bundle-update.mjs';", 'const watchForUpdate = () => {};')
    .replace("import {rendererUnavailable} from './webgpu-safety.mjs';", 'const rendererUnavailable = async () => null;')
    .replace("import {browserIdentity, browserPerson, currentIdentity, IdentityError, MAX_DISPLAY_NAME_BYTES} from './root-identity.mjs';", 'const currentIdentity = () => globalThis.__entryPageFixture.identity;')
    .replace(/from '(\.\/[^']+)'/g, (_all, path) => `from '${new URL(path, import.meta.url).href}'`);
  const {startEntryPage} = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}#${Math.random()}`);
  // A page that cannot get in throws its cause on, to the console.
  const failure = await startEntryPage({mode: 'desktop'}).then(() => null, error => error);
  await tick();
  return {calls, shown, diagnostics, page, prepared, failure};
}

test('the GPU is set up once the host lets the page in, not while it asks', async () => {
  const steps = [];
  const {prepared, failure} = await fixture({boot: async ({onStep}, page) => {
    onStep('join');
    steps.push(document.documentElement.dataset.arrival);
    onStep('open');
    onStep('open');
    return page;
  }});
  assert.deepEqual(steps, ['join']);
  assert.deepEqual(prepared, ['join'], 'prepared once, as the page moved from join to open');
  assert.equal(failure, null);
  assert.equal(document.documentElement.dataset.entry, 'open');
});

test('a page the host refuses compiles no renderer for the next page to wait on', async () => {
  const {prepared, failure} = await fixture({boot: async ({onStep}) => {
    onStep('join');
    throw new AdmissionError('no-access', 'You don’t have access to this workspace.');
  }});
  assert.equal(failure?.code, 'no-access');
  assert.equal(document.documentElement.dataset.entry, 'refused');
  assert.deepEqual(prepared, []);
});

test('rooms another host held at the origin of the page host are no longer offered', async () => {
  // The host at ORIGIN was set up again on fresh state twice since this
  // browser entered rooms there; both rooms went with the hosts before.
  const {shown} = await fixture({remembered: [
    {origin: ORIGIN, room: 'gone-1', hostDevice: 'e1'.repeat(32), name: 'Other'},
    {origin: ORIGIN, room: 'gone-2', hostDevice: 'e2'.repeat(32), name: 'Other'},
    {origin: 'https://elsewhere.example', room: 'away', hostDevice: 'e3'.repeat(32), name: 'Away'},
  ]});
  assert.deepEqual(shown.at(-1).map(action => action.title), ['Set up a machine', 'Open Other', 'Open Away', 'New workspace', 'Invite someone']);
  assert.deepEqual(enteredWorkspaces().map(entry => entry.room), ['current', 'other', 'away']);
});

test('the page keeps switch and invitation actions together and dispatches both', async () => {
  const {calls, shown, diagnostics} = await fixture();
  const actions = shown.at(-1);
  assert.deepEqual(actions.map(action => action.title), ['Set up a machine', 'Open Other', 'New workspace', 'Invite someone']);
  diagnostics.onHostAction('invite-someone');
  await tick();
  assert.deepEqual(calls, ['invite']);
  const share = document.body.children.find(form => form.dataset.card === 'share');
  assert.ok(share);
  assert.equal(share.children.find(child => child.name === 'link').value, 'https://web.example/#/current?invite=signed-secret');
  share.listeners.get('submit')({preventDefault() {}});
  diagnostics.onHostAction(actions.find(action => action.title === 'Open Other').id);
  await tick();
  assert.deepEqual(calls, ['invite', 'flush', 'stop', 'cleanup', 'dispose', 'history', 'reload']);
  assert.deepEqual(chosenWorkspace(), {origin: ORIGIN, room: 'other'});
});

test('creating a workspace stays in the current room when pending work cannot be saved', async () => {
  const {calls, diagnostics, page} = await fixture();
  page.mount.flush = async () => { calls.push('flush'); throw new Error('storage unavailable'); };
  diagnostics.onHostAction('new-workspace');
  await tick();
  const form = document.body.children.at(-1);
  form.children.find(child => child.tag === 'label').children[0].value = 'Created';
  await form.listeners.get('submit')({preventDefault() {}});
  await tick();
  assert.deepEqual(calls, ['enter', 'flush']);
  assert.equal(chosenWorkspace(), null);
  const failure = document.body.children.at(-1);
  assert.match(failure.children[0].textContent, /not saved yet/);
  failure.children.flatMap(child => [child, ...(child.children ?? [])]).find(child => child.tag === 'button').listeners.get('click')();
  await tick();
});

test('creating a workspace remembers it and uses the existing save-and-close switch path', async () => {
  const {calls, diagnostics} = await fixture();
  diagnostics.onHostAction('new-workspace');
  await tick();
  const form = document.body.children.at(-1);
  form.children.find(child => child.tag === 'label').children[0].value = 'Created';
  await form.listeners.get('submit')({preventDefault() {}});
  await tick();
  assert.deepEqual(calls, ['enter', 'flush', 'stop', 'cleanup', 'dispose', 'history', 'reload']);
  assert.deepEqual(chosenWorkspace(), {origin: ORIGIN, room: 'created'});
  assert.equal(enteredWorkspaces().find(room => room.room === 'created').name, 'Created');
});

