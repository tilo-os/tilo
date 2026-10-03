import test from 'node:test';
import assert from 'node:assert/strict';
import {openChannelVault, ChannelVaultError} from './channel-vault.mjs';

function memoryStorage() {
  const rows = new Map();
  return {
    rows,
    get: async key => rows.get(key),
    put: async (key, value) => { rows.set(key, value); },
  };
}

const user = 'ab'.repeat(32);

test('the vault is stored only sealed under a non-extractable key, and reopens', async () => {
  const storage = memoryStorage();
  const vault = await openChannelVault({profile: 'p', userPublicKey: user, storage});
  assert.equal(vault.length, 32);
  const stored = storage.rows.get(`channel-vault:p:${user}`);
  assert.equal(stored.wrapKey.extractable, false);
  await assert.rejects(crypto.subtle.exportKey('raw', stored.wrapKey));
  assert.equal(stored.sealed.length, 32 + 16, 'ciphertext and tag, no plaintext vault');
  const plain = Buffer.from(vault).toString('hex');
  assert.ok(!Buffer.from(stored.sealed).toString('hex').includes(plain));

  assert.deepEqual(await openChannelVault({profile: 'p', userPublicKey: user, storage}), vault);
});

test('each profile and user has a vault of its own', async () => {
  const storage = memoryStorage();
  const first = await openChannelVault({profile: 'p', userPublicKey: user, storage});
  const otherProfile = await openChannelVault({profile: 'q', userPublicKey: user, storage});
  const otherUser = await openChannelVault({profile: 'p', userPublicKey: 'cd'.repeat(32), storage});
  assert.notDeepEqual(first, otherProfile);
  assert.notDeepEqual(first, otherUser);
});

test('a vault moved to another profile or tampered with does not open', async () => {
  const storage = memoryStorage();
  await openChannelVault({profile: 'p', userPublicKey: user, storage});
  const stored = storage.rows.get(`channel-vault:p:${user}`);
  storage.rows.set(`channel-vault:q:${user}`, stored);
  await assert.rejects(
    openChannelVault({profile: 'q', userPublicKey: user, storage}),
    error => error instanceof ChannelVaultError && error.code === 'vault-corrupt',
  );
  const sealed = new Uint8Array(stored.sealed);
  sealed[0] ^= 1;
  storage.rows.set(`channel-vault:p:${user}`, {...stored, sealed});
  await assert.rejects(
    openChannelVault({profile: 'p', userPublicKey: user, storage}),
    error => error instanceof ChannelVaultError && error.code === 'vault-corrupt',
  );
});
