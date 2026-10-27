import assert from 'node:assert/strict';
import test from 'node:test';

import {FETCH_PROXY_PATH, fetchProxyFor, proxiedGet, wireFetchProxy} from './fetch-proxy.mjs';

const PAGE = 'https://tilo.example/';
const SITE = new URL('https://feeds.example/rss?x=1');

test('an admitted page names its host’s proxy, and every request reads the current session', () => {
  let session = 'aa11';
  const peer = {fetchProxy: () => (session ? {endpoint: `https://eu1.tilo.example${FETCH_PROXY_PATH}`, session} : null)};
  const page = {};
  wireFetchProxy(peer, page);
  assert.equal(typeof page.__tiloFetchProxy, 'function');
  assert.deepEqual(fetchProxyFor(SITE, {target: page, pageHref: PAGE}),
    {endpoint: 'https://eu1.tilo.example/peer/v1/fetch', session: 'aa11'});
  // A reconnect is a new session; between sessions there is no proxy.
  session = 'bb22';
  assert.equal(fetchProxyFor(SITE, {target: page, pageHref: PAGE}).session, 'bb22');
  session = undefined;
  assert.equal(fetchProxyFor(SITE, {target: page, pageHref: PAGE}), undefined);
});

test('a page its server gave a proxy keeps it, and a peer without one names none', () => {
  const served = {__tiloFetchProxy: '/_tilo/net/fetch'};
  wireFetchProxy({fetchProxy: () => ({endpoint: 'https://elsewhere.example/peer/v1/fetch', session: 'x'})}, served);
  assert.equal(served.__tiloFetchProxy, '/_tilo/net/fetch');
  assert.deepEqual(fetchProxyFor(SITE, {target: served, pageHref: PAGE}), {endpoint: 'https://tilo.example/_tilo/net/fetch'});

  const plain = {};
  wireFetchProxy({}, plain);
  wireFetchProxy(null, plain);
  assert.equal('__tiloFetchProxy' in plain, false);
  assert.equal(fetchProxyFor(SITE, {target: plain, pageHref: PAGE}), undefined);
});

test('same-origin requests and malformed or failing proxies go direct', () => {
  const page = {__tiloFetchProxy: () => ({endpoint: 'https://eu1.tilo.example/peer/v1/fetch', session: 's'})};
  assert.equal(fetchProxyFor(new URL('https://tilo.example/v/font.ttf'), {target: page, pageHref: PAGE}), undefined);
  for (const named of [() => { throw new Error('closed'); }, () => ({endpoint: 'javascript:alert(1)'}), () => ({}), 42]) {
    assert.equal(fetchProxyFor(SITE, {target: {__tiloFetchProxy: named}, pageHref: PAGE}), undefined);
  }
});

test('a proxied image request carries the marker and the session, never the page’s cookies', () => {
  const [url, init] = proxiedGet(SITE, {endpoint: 'https://eu1.tilo.example/peer/v1/fetch', session: 'cc33'});
  const sent = new URL(url);
  assert.equal(sent.origin + sent.pathname, 'https://eu1.tilo.example/peer/v1/fetch');
  assert.equal(sent.searchParams.get('url'), SITE.href);
  assert.deepEqual(init.headers, {'x-tilo-proxy': '1', 'x-tilo-session': 'cc33'});
  assert.equal(init.credentials, 'same-origin');
  // A host on the page's origin takes no session header.
  assert.deepEqual(proxiedGet(SITE, {endpoint: 'https://tilo.example/_tilo/net/fetch'})[1].headers, {'x-tilo-proxy': '1'});
});
