import test from 'node:test';
import assert from 'node:assert/strict';
import {
  countryName, deviceLabel, forInvites, INVITE_TTL_MS, newInvite, openInvites, PAIRING_DECLINED, PAIRING_EXPIRED,
  pairingFailure, pairSecret, parsePairSecret, whenRequested,
} from './pairing.mjs';

const USER = 'ab'.repeat(32);

test('an invite is 16 random bytes, a new one each time', () => {
  const invite = newInvite();
  assert.match(invite, /^[0-9a-f]{32}$/);
  assert.notEqual(newInvite(), invite);
  assert.equal(newInvite(bytes => bytes.fill(0xff)), 'ff'.repeat(16));
});

test('a pair secret is the person then the invite, and reads back; 64 hex names no invite', () => {
  const invite = '0f'.repeat(16);
  assert.equal(pairSecret(USER, invite), `${USER}${invite}`);
  assert.equal(pairSecret(USER), USER);
  assert.deepEqual(parsePairSecret(`${USER}${invite}`), {userPublicKey: USER, invite});
  assert.deepEqual(parsePairSecret(USER), {userPublicKey: USER, invite: null});
  for (const damaged of [undefined, '', USER.slice(1), `${USER}0f`, `${USER}${invite}0`, USER.toUpperCase(), `${USER.slice(2)}zz`]) {
    assert.equal(parsePairSecret(damaged), null, `refused: ${damaged}`);
  }
});

test('an invite is open for ten minutes, until spent, and expired ones are pruned', () => {
  let clock = 1000;
  const invites = openInvites({now: () => clock});
  const first = invites.open();
  const second = invites.open();
  assert.notEqual(first.id, second.id);
  assert.equal(first.expiresAt, 1000 + INVITE_TTL_MS);
  assert.equal(INVITE_TTL_MS, 10 * 60 * 1000);
  assert.ok(invites.has(first.id));
  invites.spend(first.id);
  assert.ok(!invites.has(first.id), 'single use');
  assert.equal(invites.size, 1);
  clock += INVITE_TTL_MS;
  assert.ok(!invites.has(second.id), 'expired');
  assert.deepEqual(invites.prune(), [second.id]);
  assert.equal(invites.size, 0);
});

test('only requests answering an open invite are this page’s, the first per invite', () => {
  const invites = openInvites();
  const mine = invites.open().id;
  const pending = [
    {digest: 'none', invite: null}, {digest: 'other', invite: 'cd'.repeat(16)},
    {digest: 'first', invite: mine}, {digest: 'second', invite: mine},
  ];
  assert.deepEqual(forInvites(pending, invites).map(request => request.digest), ['first']);
  invites.spend(mine);
  assert.deepEqual(forInvites(pending, invites), []);
});

test('a device is named by its browser and system, as people say them', () => {
  const agents = {
    'Safari on iPhone': 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1',
    'Chrome on iPhone': 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/129.0 Mobile/15E148 Safari/604.1',
    'Firefox on Linux': 'Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0',
    'Safari on Mac': 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15',
    'Chrome on Android': 'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0 Mobile Safari/537.36',
    'Edge on Windows': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0 Safari/537.36 Edg/129.0',
    'Samsung Internet on Android': 'Mozilla/5.0 (Linux; Android 14; SM-S918B) AppleWebKit/537.36 (KHTML, like Gecko) SamsungBrowser/26.0 Chrome/122.0 Mobile Safari/537.36',
  };
  for (const [label, userAgent] of Object.entries(agents)) assert.equal(deviceLabel({userAgent}), label);
  // An iPad asking for desktop sites says it is a Mac; its touch screen tells.
  assert.equal(deviceLabel({userAgent: agents['Safari on Mac'], maxTouchPoints: 5}), 'Safari on iPad');
  // Client hints, where the browser gives them, name the brand itself.
  assert.equal(deviceLabel({userAgent: agents['Chrome on Android'], userAgentData: {platform: 'macOS',
    brands: [{brand: 'Not=A?Brand'}, {brand: 'Chromium'}, {brand: 'Brave'}]}}), 'Brave on Mac');
  assert.equal(deviceLabel({userAgent: '', userAgentData: {platform: 'Windows', brands: [{brand: 'Chromium'}]}}), 'Chromium on Windows');
  assert.equal(deviceLabel({}), 'Browser');
  assert.ok(deviceLabel({userAgent: 'Firefox/1', platform: 'x'.repeat(100)}).length <= 64);
});

test('when a device asked, and from where, read as words', () => {
  assert.equal(whenRequested(1000, {now: 20_000, locale: 'en'}), 'just now');
  assert.equal(whenRequested(0, {now: 2 * 60_000, locale: 'en'}), '2 minutes ago');
  assert.equal(whenRequested(0, {now: 3 * 3_600_000, locale: 'en'}), '3 hours ago');
  assert.equal(countryName('DE', {locale: 'en'}), 'Germany');
  assert.equal(countryName('de', {locale: 'de'}), 'Deutschland');
  assert.equal(countryName(null), null);
  assert.equal(countryName('not a region', {locale: 'en'}), 'not a region');
});

test('the host’s refusals of a pairing are said as what to do next', () => {
  assert.equal(pairingFailure(new Error('certification denied: declined by the approving device')), PAIRING_DECLINED);
  assert.equal(pairingFailure(new Error('certification transport: this pairing link was used already; make a new one')), PAIRING_EXPIRED);
  assert.equal(pairingFailure(new Error('no approver answered the certification request')), PAIRING_EXPIRED);
  assert.equal(PAIRING_EXPIRED, 'This link expired or was used already. Make a new one on your other device.');
  assert.equal(PAIRING_DECLINED, 'Your other device declined. Make a new link to try again.');
  assert.equal(pairingFailure(new TypeError('Failed to fetch')), null);
});
