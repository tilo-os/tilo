// Exercise a real single-Chat WASM bundle, not a mocked browser mount.
import assert from 'node:assert/strict';
import {resolve} from 'node:path';
import {randomUUID} from 'node:crypto';
import {serve, launch} from '../../tools/web/chrome.mjs';

const bundle = process.argv[2];
assert.ok(bundle, 'supply the normal tilo-app Chat web bundle directory');
let browser, server;
try {
  server = await serve(resolve(bundle));
  browser = await launch({args: ['--use-angle=swiftshader', '--enable-features=Vulkan']});
  const tab = await browser.tab();
  const room = randomUUID();
  await tab.navigate(`${server.url}/app/?room=${room}#/me/so.tilo.chat`);
  await tab.waitFor('globalThis.tiloMount?.state().phase === "presented"');
  const journey = await tab.evaluate(`(() => {
    const mount = globalThis.tiloMount;
    const check = (value, message) => { if (!value) throw new Error(message); };
    const invoke = (resource, action, arguments_ = {}) => {
      const result = mount.invokeAction(resource, action, arguments_);
      check(result.state === 'completed', action + ' must complete');
      return result.output;
    };
    const catalogue = () => mount.publications();
    const workspace = catalogue().find(row => row.address.action === 'new-channel');
    check(workspace, 'actual Chat workspace must publish new-channel');
    const channel = name => {
      invoke(workspace.semantic_identity.key, 'new-channel', {value: name});
      const row = catalogue().find(row => row.resource_kind === 'chat.channel'
        && row.resource_label === '#' + name && row.address.action === 'post');
      check(row, 'created channel must be published');
      return row.semantic_identity.key;
    };
    const first = channel('browser-first');
    const second = channel('browser-second');
    invoke(first, 'post', {value: 'Browser fixture message'});
    const reference = {kind: 'book', id: 'fixture-book', label: 'Fixture book', span: null};
    invoke(first, 'share-resource', {reference, text: 'Live resource fixture',
      sourceProvider: 'record:so.tilo.books:fixture-record'});
    invoke(first, 'mark-read');
    const readFirst = invoke(first, 'read-messages', {pageSize: 32});
    const readSecond = invoke(second, 'read-messages', {pageSize: 32});
    check(readFirst.value.content.messages.length === 2, 'exactly two messages must be readable');
    check(readFirst.value.content.messages.filter(message => message.text === 'Browser fixture message').length === 1,
      'posted text must occur exactly once');
    check(readSecond.value.content.messages.length === 0, 'other conversation must stay empty');
    const shared = catalogue().find(row => row.address.action === 'read-message'
      && row.resource_label.includes('Live resource fixture'));
    check(shared, 'shared resource message must publish its read action');
    const readShared = invoke(shared.semantic_identity.key, 'read-message');
    check(Object.keys(readShared.value.reference).sort().join(',') === Object.keys(reference).sort().join(',')
      && Object.entries(reference).every(([key, value]) => readShared.value.reference[key] === value),
      'message read must preserve the exact typed reference');
    check(readShared.value.sourceProvider === 'record:so.tilo.books:fixture-record',
      'message read must preserve provider provenance');
    check('chat.channel:' + readShared.value.channel === first, 'resource message must belong to first conversation');
    check(!JSON.stringify(readSecond).includes('Browser fixture message'), 'message must stay in its conversation');
    check(!JSON.stringify(readSecond).includes('fixture-book'), 'reference must stay in its conversation');
    check(mount.state().frames > 0, 'real compiled frame must be presented');
    return {first, second, shared: shared.semantic_identity.key, readShared, readFirst, readSecond, frames: mount.state().frames};
  })()`);
  await tab.evaluate('tiloMount.flush()');
  await tab.reload();
  await tab.waitFor('globalThis.tiloMount?.state().phase === "presented"');
  const restored = await tab.evaluate(`tiloMount.invokeAction(${JSON.stringify(journey.first)}, 'read-messages', {pageSize: 32})`);
  assert.equal(restored.state, 'completed');
  assert.deepEqual(restored.output, journey.readFirst, 'durable reload preserves the complete read result');
  const sharedRestored = await tab.evaluate(`tiloMount.invokeAction(${JSON.stringify(journey.shared)}, 'read-message')`);
  assert.deepEqual(sharedRestored.output, journey.readShared, 'durable reload preserves the exact typed reference and provider');
  console.log(JSON.stringify({mode: 'compiled-chat-browser-local', qualifiedRemotePeer: false,
    frames: journey.frames, conversationIsolation: true, typedReference: true, durableReload: true}));
} finally {
  if (browser) await browser.close();
  if (server) await server.close();
}
