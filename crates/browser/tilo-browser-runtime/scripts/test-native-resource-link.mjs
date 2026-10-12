/** Pair an observed native encoder result with the production browser resolver.
 * This fixture contains no live credential or runtime storage. It does not prove
 * a compositor/browser deployment or the compiled WASM mount itself. */
import {readFile} from 'node:fs/promises';
import assert from 'node:assert/strict';
import {parseResourceLink} from '../web/resource-link.mjs';
import {openResourceReference} from '../web/resource-reference.mjs';
import {openResourceOperation, closeResourceOperation} from '../web/resource-session.mjs';
const log = await readFile(process.argv[2], 'utf8');
const link = log.match(/^NATIVE_RESOURCE_LINK=(https:\/\/\S+)$/mu)?.[1];
assert.ok(link, 'The native production encoder must have been observed');
const address = parseResourceLink(link);
assert.equal(address.reference.kind, 'chat.message');
assert.deepEqual(address.reference.span, {start: 6, end: 7, unit: 'Chars'});
assert.equal(address.action, 'reveal-message');
const range = {action: 'reveal-message-range', start_param: 'start', end_param: 'end', unit_param: 'unit'};
const view = {kind: 'View', address: {resource: address.reference.id, action: address.action},
  view: {open_action: address.action, range}, params: [], contract: {}};
const rangeView = {kind: 'View', address: {resource: address.reference.id, action: range.action},
  params: ['start', 'end', 'unit'].map((name, index) => ({name, ty: index < 2 ? 'integer' : 'text', required: true})), contract: {}};
const rows = [view, rangeView].map(action => ({provider: address.provider, app_id: 'so.tilo.chat',
  resource: {id: address.reference.id, kind: address.reference.kind, label: 'Current publisher label'}, action}));
const scope = {origin: address.origin, room: address.room, hostDevice: address.hostDevice};
async function journey(revoke = false) {
  const steps = []; let admitted = true; let queries = 0;
  const actions = {catalogue: async query => {
    assert.equal(query.provider, address.provider);
    assert.deepEqual(query.view_resource, {id: address.reference.id, kind: address.reference.kind});
    queries++;
    return {rows: revoke && queries > 1 ? [] : rows, locator: {...scope, provider: address.provider}};
  }};
  const mount = {actions: () => [view, rangeView], invokeAction: (...args) => {
    steps.push(['invoke', ...args]); return {state: 'completed'};
  }, cleanup: async () => steps.push(['cleanup'])};
  const peer = {resourceScope: scope, whenReady: async () => assert.ok(admitted),
    openResource: async (_target, reference, provider, action, {signal}) => openResourceReference({
      reference, provider, action, actions, scope, signal, current: () => admitted,
      waitRecord: async (app, record) => steps.push(['hydrate', app, record]),
      mount: async () => mount,
    }), dispose: async () => { admitted = false; steps.push(['dispose']); }};
  const operation = {controller: new AbortController()};
  const opening = openResourceOperation({operation, link, target: 'fixture', connect: async () => peer});
  if (revoke) {
    await assert.rejects(opening, /unavailable|permitted/);
    assert.deepEqual(steps, [['hydrate', 'so.tilo.chat', 'discussion'], ['cleanup'], ['dispose']]);
    assert.equal(operation.controller.signal.aborted, true);
  } else {
    await opening;
    assert.deepEqual(steps, [['hydrate', 'so.tilo.chat', 'discussion'],
      ['invoke', address.reference.id, 'reveal-message-range', {start: 6, end: 7, unit: 'Chars'}]]);
    await closeResourceOperation(operation);
    assert.deepEqual(steps.slice(-2), [['cleanup'], ['dispose']]);
    assert.equal(operation.controller.signal.aborted, true);
  }
}
await journey(); await journey(true);
console.log('Observed native link -> exact browser hydration/range view -> Leave and post-hydration revocation: passed');
