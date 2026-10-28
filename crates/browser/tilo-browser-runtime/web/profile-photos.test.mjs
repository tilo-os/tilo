import test from 'node:test';
import assert from 'node:assert/strict';
import {followPhotos} from './profile-photos.mjs';

const ADA = 'a'.repeat(64);
const BO = 'b'.repeat(64);
const settle = () => new Promise(resolve => setTimeout(resolve, 0));

test('a person’s picture is fetched when their profile changes, and only then', async () => {
  const fetched = [];
  const shown = [];
  let photo = 'data:image/webp;base64,AAAA';
  const photos = followPhotos({
    profileOf: async user => { fetched.push(user); return {user_public_key: user, display_name: 'Ada', ...(photo ? {photo} : {})}; },
    show: async (user, url) => { shown.push([user, url]); },
  });
  photos.seen([{user: ADA, profile_ms: 10}, {user: BO, profile_ms: 0}, {user: 'not-a-key', profile_ms: 5}]);
  await settle();
  assert.deepEqual(fetched, [ADA], 'nobody without a kept profile is asked about');
  assert.deepEqual(shown, [[ADA, 'data:image/webp;base64,AAAA']]);
  photos.seen([{user: ADA, profile_ms: 10}]);
  await settle();
  assert.equal(fetched.length, 1, 'the same profile is not fetched again');
  photo = null;
  photos.seen([{user: ADA, profile_ms: 20}]);
  await settle();
  assert.deepEqual(shown.at(-1), [ADA, null], 'a picture taken away goes');
});
