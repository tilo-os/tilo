import test from 'node:test';
import assert from 'node:assert/strict';
import {createResourceLink, parseResourceLink, openResourceLink} from './resource-link.mjs';
const scope = {origin: 'https://host.example', room: 'admitted-room', hostDevice: 'ab'.repeat(32)};
const reference = {kind: 'chat.message', id: 'chat.message:m1', label: 'untrusted', span: {start: 1, end: 4, unit: 'Chars'}};
const provider = 'record:so.tilo.chat:uuid:1';
const view = {address: {action: 'reveal'}, view: {range: {action: 'range'}}};
const peer = {resourceScope: scope, resolveResource: async () => ({views: [view], locator: {...scope, provider}}), whenReady: async () => {}};
const link = () => createResourceLink({peer, page: 'https://web.example/resource.html?token=discard', reference, provider, action: 'reveal'});
test('an admitted publisher creates a fragment address with no label, account or grant', async () => {
  const url = new URL(await link());
  assert.equal(url.search, '');
  const address = parseResourceLink(url);
  assert.deepEqual(address, {...scope, version: 1, reference: {kind: reference.kind, id: reference.id, span: reference.span}, provider, action: 'reveal'});
  assert.ok(!url.href.includes('untrusted'));
  await assert.rejects(createResourceLink({peer: {...peer, resolveResource: async () => ({views: [], locator: {...scope, provider}})},
    page: url, reference, provider, action: 'reveal'}), /unavailable/);
});
test('a recipient opens only in the matching admitted host and room', async () => {
  const calls = [];
  const recipient = {...peer, openResource: async (...args) => { calls.push(args); return 'mounted'; }};
  assert.equal(await openResourceLink({peer: recipient, link: await link(), target: 'surface'}), 'mounted');
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].slice(0, 4), ['surface', {kind: reference.kind, id: reference.id, span: reference.span}, provider, 'reveal']);
  for (const changed of [{origin: 'https://other.example'}, {room: 'other'}, {hostDevice: 'cd'.repeat(32)}]) {
    await assert.rejects(openResourceLink({peer: {...recipient, resourceScope: {...scope, ...changed}}, link: await link(), target: 'surface'}), /another host or room/);
  }
  assert.equal(calls.length, 1);
});
test('credentials, remote plain HTTP, malformed ranges and over-budget links refuse', async () => {
  const valid = await link();
  for (const patch of [{origin: 'https://user:password@host.example'}, {origin: 'http://host.example'},
    {provider: 'workspace:any'}, {reference: {...reference, span: {start: 2, end: 1, unit: 'Chars'}}}]) {
    const payload = {...parseResourceLink(valid), ...patch};
    assert.throws(() => parseResourceLink('https://web.example/resource.html#resource=' + encodeURIComponent(JSON.stringify(payload))));
  }
  assert.throws(() => parseResourceLink('https://web.example/#resource=' + 'x'.repeat(24_000)), /Invalid/);
});

test('link minting freezes the address and refuses changed admitted scope', async () => {
  const mutable = {...reference, span: {...reference.span}};
  const changing = {...peer, resolveResource: async address => {
    mutable.id = 'different'; mutable.span.end = 999;
    assert.equal(address.id, reference.id); assert.equal(address.span.end, 4);
    return {views: [view], locator: {...scope, provider}};
  }};
  const minted = await createResourceLink({peer: changing, page: 'https://web.example/resource.html',
    reference: mutable, provider, action: 'reveal'});
  assert.equal(parseResourceLink(minted).reference.id, reference.id);
  assert.equal(parseResourceLink(minted).reference.span.end, 4);
  let scopeNow = scope;
  const moved = {get resourceScope() { return scopeNow; }, resolveResource: async () => {
    scopeNow = {...scope, room: 'replacement'}; return {views: [view], locator: {...scope, provider}};
  }};
  await assert.rejects(createResourceLink({peer: moved, page: 'https://web.example/resource.html',
    reference, provider, action: 'reveal'}), /connection changed/);
});

test('a copied address requires the exact fresh host-issued locator', async () => {
  for (const locator of [undefined, {...scope, provider: 'record:other:uuid:1'},
    {...scope, provider, room: 'other'}, {...scope, provider, hostDevice: 'cd'.repeat(32)},
    {...scope, provider, origin: 'https://other.example'}]) {
    await assert.rejects(createResourceLink({peer: {...peer,
      resolveResource: async () => ({views: [view], locator})},
      page: 'https://web.example/resource.html', reference, provider, action: 'reveal'}),
      /host-issued locator/);
  }
});
