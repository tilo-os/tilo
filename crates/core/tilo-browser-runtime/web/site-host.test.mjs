import assert from 'node:assert/strict';
import test from 'node:test';
import {forgetSiteServers, isInvitedServer, invitedOrigin, nameInvitedServer, nameSiteServer, siteNames, siteServer} from './site-host.mjs';

/** A browser's storage, as `localStorage` answers. */
function storage() {
  const kept = new Map();
  return {getItem: key => kept.get(key) ?? null, setItem: (key, value) => kept.set(key, String(value))};
}

const info = {hostCert: {device_public_key: [1, 2, 3]}, address: {endpointAddr: {id: 'f978', addrs: []}, relayUrl: 'https://live-eu1.tilo.sh/', web: null}};

test('a server an invitation names is named under an origin that never resolves, and kept for the next visit', () => {
  const origin = invitedOrigin('F97887D9895250EF7D60F7929CC947CCC4E4C668B606D2933867E7189778563F');
  assert.equal(origin, 'https://f97887d9895250ef7d60f7929cc947cc.iroh.invalid');
  assert.ok(isInvitedServer(origin));
  assert.ok(!isInvitedServer('https://eu1.tilo.sh'));
  const store = storage();
  nameInvitedServer(origin, info, store);
  assert.deepEqual(siteServer(origin, store).info, info);
  // The next visit knows it from what this browser kept alone.
  forgetSiteServers();
  assert.deepEqual(siteServer(origin, store).info, info);
  forgetSiteServers();
  assert.equal(siteServer(origin, storage()), null, 'another browser knows nothing of it');
  forgetSiteServers();
});

test('what is not a server’s address is not named', () => {
  const store = storage();
  nameInvitedServer(invitedOrigin('ab'), {hostCert: {}}, store);
  assert.equal(siteServer(invitedOrigin('ab'), store), null);
  assert.equal(invitedOrigin(''), null);
});

test('a server the site names is known by the origin the site names it at, whatever else names it', () => {
  // An invitation names its server by its own signed address; when that is the server the site
  // names (the Lounge's own server), the page dials it at the site's origin for it, never under a
  // name only this browser would know.
  const home = {...info, hostCert: {device_public_key: [9, 9, 9]}};
  nameSiteServer({host: {origin: 'https://eu1.tilo.sh/', info: JSON.stringify(home)}});
  const store = storage();
  nameInvitedServer(invitedOrigin('f978'), info, store);
  assert.equal(siteNames({...home, address: {endpointAddr: {id: 'other', addrs: []}}}), 'https://eu1.tilo.sh');
  assert.equal(siteNames(info), null, 'a server only an invitation named is not one the site names');
  assert.equal(siteNames({hostCert: {device_public_key: [4]}}), null);
  assert.equal(siteNames(null), null);
  forgetSiteServers();
});
