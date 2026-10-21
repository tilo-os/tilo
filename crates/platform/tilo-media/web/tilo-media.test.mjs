import test from 'node:test';
import assert from 'node:assert/strict';
import {proxiedMediaUrl} from './tilo-media.mjs';

const page = {location: {href: 'https://tilo.coffee/lounge'}};
const video = 'https://rr1.googlevideo.com/videoplayback?itag=18&sig=a%2Bb';

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
