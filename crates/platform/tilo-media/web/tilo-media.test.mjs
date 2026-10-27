import test from 'node:test';
import assert from 'node:assert/strict';
import {TiloMediaElement, proxiedMediaUrl, siteOf, lacksCors} from './tilo-media.mjs';

const page = {location: {href: 'https://tilo.coffee/lounge'}};
const video = 'https://rr1.googlevideo.com/videoplayback?itag=18&sig=a%2Bb';
// A source on a site not known to refuse the page: tried directly first.
const film = 'https://cdn.example.net/films/a.mp4';

test('a source without CORS plays through the host the page got into', () => {
  const scope = {...page, __tiloFetchProxy: () => ({endpoint: 'https://eu1.tilo.coffee/peer/v1/fetch', session: 'ab12'})};
  const proxied = new URL(proxiedMediaUrl(video, scope));
  assert.equal(proxied.origin + proxied.pathname, 'https://eu1.tilo.coffee/peer/v1/fetch');
  // The element cannot send headers: marker and session ride in the query,
  // and the target keeps its own query intact.
  assert.equal(proxied.searchParams.get('url'), video);
  assert.equal(proxied.searchParams.get('tilo-proxy'), '1');
  assert.equal(proxied.searchParams.get('tilo-session'), 'ab12');
});

test('a development server on the page origin names its endpoint alone', () => {
  const scope = {location: {href: 'http://127.0.0.1:8787/'}, __tiloFetchProxy: '/_tilo/net/fetch'};
  const proxied = new URL(proxiedMediaUrl(video, scope));
  assert.equal(proxied.href.split('?')[0], 'http://127.0.0.1:8787/_tilo/net/fetch');
  assert.equal(proxied.searchParams.get('tilo-session'), null);
});

test('without a proxy, or for a source that needs none, the element loads it itself', () => {
  assert.equal(proxiedMediaUrl(video, page), null);
  const scope = {...page, __tiloFetchProxy: () => null};
  assert.equal(proxiedMediaUrl(video, scope), null);
  const named = {...page, __tiloFetchProxy: '/_tilo/net/fetch'};
  assert.equal(proxiedMediaUrl('https://tilo.coffee/media/a.mp4', named), null, 'same origin');
  assert.equal(proxiedMediaUrl('blob:https://tilo.coffee/1234', named), null, 'a page-local blob');
  assert.equal(proxiedMediaUrl('data:video/mp4;base64,AAAA', named), null);
});

// A stand-in for the page's media element: its source, and the listeners
// the engine adds.
function fakePage(proxyError) {
  const listeners = {};
  const el = {
    src: '', currentTime: 0, error: null, style: {}, paused: true,
    addEventListener: (name, listener) => { listeners[name] = listener; },
    load() {}, play: () => Promise.resolve(), pause() {}, remove() {}, removeAttribute() {},
  };
  const asked = [];
  globalThis.document = {createElement: () => el, body: {appendChild() {}}};
  globalThis.location = page.location;
  globalThis.__tiloFetchProxy = () => ({endpoint: 'https://eu1.tilo.coffee/peer/v1/fetch', session: 'ab12'});
  globalThis.fetch = async (url, init) => {
    asked.push([url, init.method]);
    return {headers: {get: name => (name === 'x-tilo-proxy-error' ? proxyError : null)}};
  };
  const fail = () => { el.error = {code: 2}; return listeners.error(); };
  const played = () => listeners.loadedmetadata?.();
  return {el, asked, fail, played};
}

test('a site known to refuse the page loads through the proxy from the start', t => {
  t.after(() => { delete globalThis.document; delete globalThis.location; delete globalThis.__tiloFetchProxy; });
  const {el} = fakePage(null);
  assert.equal(siteOf(video, page), 'googlevideo.com');
  assert.ok(lacksCors('https://rr10---sn-abc.googlevideo.com/videoplayback?id=1', page));
  const media = new TiloMediaElement();
  media.open(video);
  const proxied = new URL(el.src);
  assert.equal(proxied.origin, 'https://eu1.tilo.coffee', 'no direct request that is refused first');
  assert.equal(proxied.searchParams.get('url'), video);
});

test('a site that refused the page and played through the proxy goes there first next time', t => {
  t.after(() => { delete globalThis.document; delete globalThis.location; delete globalThis.__tiloFetchProxy; });
  const {el, fail, played} = fakePage(null);
  const media = new TiloMediaElement();
  const first = 'https://a1.media.example.org/one.mp4';
  media.open(first);
  assert.equal(el.src, first, 'tried directly while nothing is known of the site');
  fail();
  assert.equal(new URL(el.src).origin, 'https://eu1.tilo.coffee');
  played();
  media.open('https://b2.media.example.org/two.mp4');
  assert.equal(new URL(el.src).origin, 'https://eu1.tilo.coffee', 'another host of the same site');
  // A source that plays directly teaches nothing.
  media.open(film);
  assert.equal(el.src, film);
  played();
  assert.equal(lacksCors(film, page), false);
});

test('a host that streamed all it allows says so, and the source is not loaded again', async t => {
  t.after(() => { delete globalThis.document; delete globalThis.location; delete globalThis.__tiloFetchProxy; });
  const {el, asked, fail} = fakePage('over-budget');
  const media = new TiloMediaElement();
  media.open(film);
  fail();
  assert.equal(new URL(el.src).origin, 'https://eu1.tilo.coffee', 'the refused source loads through the proxy');
  assert.equal(asked.length, 0, 'a direct load that fails asks the proxy nothing');
  const proxied = el.src;
  await media.explainProxied(proxied, {code: 2});
  assert.deepEqual(asked, [[proxied, 'HEAD']]);
  assert.equal(el.src, proxied, 'the spent budget is not asked again');
  assert.equal(media.takeLimitReached(), true);
  assert.match(media.takeError(), /as much as it allows/);
  assert.equal(media.takeLimitReached(), false);
  // A new source starts without the old reason.
  media.open(film);
  assert.equal(media.takeLimitReached(), false);
});

test('a proxied load that failed for another reason is loaded again, then reported', async t => {
  t.after(() => { delete globalThis.document; delete globalThis.location; delete globalThis.__tiloFetchProxy; });
  const {el, fail} = fakePage(null);
  const media = new TiloMediaElement();
  media.open(film);
  fail();
  const proxied = el.src;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    el.error = {code: 2};
    await media.explainProxied(media.proxied, el.error);
  }
  assert.equal(el.src, proxied);
  assert.equal(media.takeLimitReached(), false);
  assert.equal(media.takeError(), 'network error');
});
