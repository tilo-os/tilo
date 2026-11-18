import test from 'node:test';
import assert from 'node:assert/strict';
import {
  deviceLabel, forInvites, INVITE_TTL_MS, newInvite, openInvites, PAIRING_CLOCK, PAIRING_DECLINED, PAIRING_UNANSWERED,
  PAIRING_SAME_PERSON, PAIRING_USED, PAIRING_WAIT, pairingFailure, pairingStatus, pairSecret, parsePairSecret, whenRequested,
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

test('a native app that holds the page names its device itself', () => {
  const safari = {userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15'};
  assert.equal(deviceLabel(safari, {name: 'Tilo Assistant on Ada’s Mac'}), 'Tilo Assistant on Ada’s Mac');
  // One line of at most 64 characters, whatever it was handed.
  assert.equal(deviceLabel(safari, {name: '  Tilo\nAssistant\u0000 '}), 'Tilo Assistant');
  assert.equal(deviceLabel(safari, {name: 'x'.repeat(100)}), 'x'.repeat(64));
  // No name, an empty one or something that is no text leaves it to the browser and the system.
  for (const none of [{}, {name: ''}, {name: ' \n '}, {name: 42}, {name: {toString: () => 'x'}}, null]) {
    assert.equal(deviceLabel(safari, none), 'Safari on Mac');
  }
});

test('when a device asked reads as words', () => {
  assert.equal(whenRequested(1000, {now: 20_000, locale: 'en'}), 'just now');
  assert.equal(whenRequested(0, {now: 2 * 60_000, locale: 'en'}), '2 minutes ago');
  assert.equal(whenRequested(0, {now: 3 * 3_600_000, locale: 'en'}), '3 hours ago');
});

test('the host’s refusals of a pairing are said as what to do next', () => {
  assert.equal(pairingFailure(new Error('certification denied: declined by the approving device')), PAIRING_DECLINED);
  assert.equal(pairingFailure(new Error('certification transport: this pairing link was used already; make a new one')), PAIRING_USED);
  assert.equal(pairingFailure(new Error('no approver answered the certification request')), PAIRING_UNANSWERED);
  assert.equal(pairingFailure(new Error('no approving device answered in time')), PAIRING_UNANSWERED);
  assert.equal(PAIRING_USED, 'This link was used already. Make a new one on your other device.');
  assert.equal(PAIRING_UNANSWERED, 'No device of yours approved this one in time. Make a new link on your other device, then approve this device there.');
  assert.equal(PAIRING_DECLINED, 'Your other device declined. Make a new link to try again.');
  assert.equal(pairingFailure(new TypeError('Failed to fetch')), null);
  // This browser's own refusal, by its code: a link for the person it already is.
  assert.equal(pairingFailure(Object.assign(new Error('refused here'), {code: 'same-person'})), PAIRING_SAME_PERSON);
  assert.equal(PAIRING_SAME_PERSON, 'This browser is already signed in as you, so nothing was paired. Open the link on the device you want to add.');
});

test('a pairing the host refuses for the device’s clock says so, never that the link expired', () => {
  // What a host says of a request its clock is behind (20 ms was enough), as
  // hosts worded it before they allowed a difference, and as they word it now
  // for a clock more than a minute ahead or a request that arrives expired.
  for (const refusal of [
    'certification request is expired or not yet valid',
    'certification request is dated more than 60 seconds ahead: the clocks of the two devices differ',
    'certification request has expired',
  ]) {
    const said = pairingFailure(new Error(refusal));
    assert.equal(said, PAIRING_CLOCK, refusal);
    assert.doesNotMatch(said, /expired|used already/i, 'nothing happened to the link');
  }
  assert.equal(PAIRING_CLOCK, 'The clock of this device and the clock of your workspace’s host differ by more than a minute. Check the date and time on this device, then open the link again.');
});

test('a waiting device is told where it is approved and where “Pair a new device” is found', () => {
  assert.deepEqual({...PAIRING_WAIT}, {
    heading: 'Waiting for approval on your other device…',
    where: 'The device that made this link now asks you about this one. Approve there if it shows this code:',
    then: 'This page goes on by itself once you approve.',
    otherwise: 'Nothing asking there? Its link was made more than 10 minutes ago, or its page was closed. On a device where you are signed in, choose “Pair a new device” on your own card (your picture, at the top right) or in search, and open the new link here.',
  });
  assert.equal(INVITE_TTL_MS, 10 * 60 * 1000, 'the minutes the sentence names');
  assert.equal(pairingStatus('123456'), 'Pairing code 123456. This tab is a new device of yours and waits for your approval. On a device where you are signed in, approve 123456 where it asks. If nothing asks, choose “Pair a new device” there on your own card (your picture, at the top right) or in search, and open its link in this tab.');
});

test('a device says whose it becomes in one sentence, with the name its host gave and nothing that could bend it', async () => {
  const pairing = await import('./pairing.mjs');
  // The name is the word of the site's own server, and of no other.
  const own = {ofSite: true};
  assert.equal(pairing.pairingWhose('Ada Lovelace', own), 'You are adding this device to Ada Lovelace’s devices. If that is not you, stop.');
  // No name from the host: it says so, and what to go by instead.
  for (const none of ['', null, undefined, '   ', '‮‏']) {
    assert.equal(pairing.pairingWhose(none, own),
      'You are adding this device to the devices of someone this host has no name for. If you did not make this link yourself, stop.');
  }
  // A name cannot turn the sentence round, break its line or run on.
  assert.equal(pairing.pairingWhose('Bo‮\n  Smith⁦‏', own), 'You are adding this device to Bo Smith’s devices. If that is not you, stop.');
  assert.equal(Array.from(pairing.shownName('x'.repeat(200))).length, 64);
  // A server the site does not name as its own names nobody, whatever it
  // answered, and so does a caller that did not say whose the server is.
  for (const whose of [undefined, {}, {ofSite: false}, {ofSite: 'yes'}, {ofSite: 1}]) {
    for (const name of ['Ada Lovelace', '', null]) {
      assert.equal(pairing.pairingWhose(name, whose),
        'You are adding this device to the devices of whoever made this link. If you did not make it yourself, stop.');
    }
  }
});

test('a device that stopped is told that nothing was paired', async () => {
  const pairing = await import('./pairing.mjs');
  assert.equal(pairing.pairingFailure(Object.assign(new Error('stopped here'), {code: 'stopped'})), pairing.PAIRING_STOPPED);
  assert.match(pairing.PAIRING_STOPPED, /^Nothing was paired\./);
});

test('a pairing a tab asked for itself is known as its own once, and a link it did not ask for never is', async () => {
  const {askOwnPairing, ownPairing} = await import('./pairing.mjs');
  const kept = new Map();
  const storage = {getItem: key => kept.get(key) ?? null, setItem: (key, value) => kept.set(key, value), removeItem: key => kept.delete(key)};
  const invite = '0f'.repeat(16);
  askOwnPairing(USER, invite, storage);
  assert.equal(ownPairing(USER, invite, storage), true);
  assert.equal(ownPairing(USER, invite, storage), false, 'read once');
  askOwnPairing(USER, invite, storage);
  assert.equal(ownPairing('cd'.repeat(32), invite, storage), false, 'another person');
  askOwnPairing(USER, invite, storage);
  assert.equal(ownPairing(USER, 'aa'.repeat(16), storage), false, 'another invite');
  assert.equal(ownPairing(USER, invite, storage), false, 'and a mismatch spends it too');
  assert.equal(ownPairing(USER, invite, null), false, 'no storage: nothing asked here');
  const broken = {getItem: () => { throw new Error('blocked'); }, setItem: () => { throw new Error('blocked'); }, removeItem: () => {}};
  askOwnPairing(USER, invite, broken);
  assert.equal(ownPairing(USER, invite, broken), false);
});
