import test from 'node:test';
import assert from 'node:assert/strict';
import {fetchPersonProfile, followPhotos} from './profile-photos.mjs';
import {invitedOrigin} from './site-host.mjs';

const ADA = 'a'.repeat(64);
const BO = 'b'.repeat(64);
const settle = () => new Promise(resolve => setTimeout(resolve, 0));

test('a person’s picture is fetched when they are met and when their profile changes, and only then', async () => {
  const fetched = [];
  const shown = [];
  let photo = 'data:image/webp;base64,AAAA';
  const photos = followPhotos({
    profileOf: async user => { fetched.push(user); return {user_public_key: user, display_name: 'Ada', ...(photo ? {photo} : {})}; },
    show: async (user, url) => { shown.push([user, url]); },
  });
  photos.seen([{user: ADA, profile_ms: 10}, {user: BO, profile_ms: 0}, {user: 'not-a-key', profile_ms: 5}]);
  await settle();
  assert.deepEqual(fetched, [ADA, BO], 'everyone met is asked about once, a stamp or not');
  assert.deepEqual(shown, [[ADA, 'data:image/webp;base64,AAAA'], [BO, 'data:image/webp;base64,AAAA']]);
  photos.seen([{user: ADA, profile_ms: 10}, {user: BO, profile_ms: 0}]);
  await settle();
  assert.equal(fetched.length, 2, 'the same profile is not fetched again');
  photos.seen([{user: ADA, profile_ms: 5}]);
  await settle();
  assert.equal(fetched.length, 2, 'a device that has not heard of the change yet changes nothing');
  photo = null;
  photos.seen([{user: ADA, profile_ms: 20}]);
  await settle();
  assert.deepEqual(shown.at(-1), [ADA, null], 'a picture taken away goes');
});

test('a profile is asked of a server with a web origin, never of one only an invitation named', async () => {
  const asked = [];
  const fetcher = async url => {
    asked.push(url);
    return {ok: true, json: async () => ({profile: {user_public_key: ADA, display_name: 'Ada'}, issued_at_ms: 7})};
  };
  assert.deepEqual(await fetchPersonProfile('https://eu1.tilo.sh', ADA, fetcher),
    {user_public_key: ADA, display_name: 'Ada', issued_at_ms: 7});
  assert.equal(await fetchPersonProfile(invitedOrigin('0221e6218cbf08506ee73f14758b8f79'), ADA, fetcher), null);
  assert.deepEqual(asked, ['https://eu1.tilo.sh/peer/v1/profile/fetch'], 'the unresolvable origin is never fetched');
});
