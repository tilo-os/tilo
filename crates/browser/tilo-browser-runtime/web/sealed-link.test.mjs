import assert from 'node:assert/strict';
import test from 'node:test';
import {sealedHandle, sealedLink} from './sealed-link.mjs';

const HANDLE = 'abcdefghijklmnop';
const KEY = 'A'.repeat(43);
const at = (pathname, hash = '') => ({pathname, href: `https://tilo.sh${pathname}${hash}`});

test('a sealed link is the site, the handle in its path and the key in its fragment', () => {
  assert.equal(sealedHandle(`/i/${HANDLE}`), HANDLE);
  assert.deepEqual(sealedLink({location: at(`/i/${HANDLE}`, `#${KEY}`), secrets: {invite: KEY}}),
    {site: 'https://tilo.sh', handle: HANDLE, key: KEY});
  // The link names the site, whatever else its address carries.
  assert.equal(sealedLink({location: {pathname: `/i/${HANDLE}`, href: `https://tilo.sh/i/${HANDLE}?from=chat#${KEY}`},
    secrets: {invite: KEY}}).site, 'https://tilo.sh');
  assert.equal(sealedLink({location: {pathname: `/i/${HANDLE}`, href: `https://tilo.sh:8443/i/${HANDLE}#${KEY}`},
    secrets: {invite: KEY}}).site, 'https://tilo.sh:8443');
});

test('a handle that is not one, or a page that is not sealed, is no sealed link', () => {
  for (const pathname of ['/', '/i/', '/i/short', `/i/${HANDLE}/`, `/i/${HANDLE}x`, `/i/${'A'.repeat(17)}`,
    `/x/${HANDLE}`, `/i/${'a'.repeat(15)}$`]) {
    assert.equal(sealedHandle(pathname), null, pathname);
    assert.equal(sealedLink({location: at(pathname, `#${KEY}`), secrets: {invite: KEY}}), null, pathname);
  }
  assert.equal(sealedHandle(undefined), null);
  // The key leaves the address before the page runs (the scrubber); a page
  // that kept none has nothing to open the invitation with.
  assert.equal(sealedLink({location: at(`/i/${HANDLE}`), secrets: {}}), null);
  assert.equal(sealedLink({location: at(`/i/${HANDLE}`), secrets: {invite: 'short'}}), null);
  // A bare key without the sealed route is no invitation, whatever else the
  // link said.
  assert.equal(sealedLink({location: at('/'), secrets: {invite: KEY}}), null);
});
