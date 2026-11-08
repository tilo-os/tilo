/**
 * An unlocked identity with the calls the page makes on one, recording them:
 * a stand-in where the real identity (which needs the wasm runtime and a
 * `WebCrypto` custody key) is more than a page test needs.
 *
 * `envelope` starts empty and grows a wrap per keeping call, as the real one
 * does; `calls` records what was asked (`['keepWithPasskey', options]`,
 * `['createRecoveryKey']`).
 */
export function fakeIdentity({userPublicKey = 'ab'.repeat(32)} = {}) {
  const calls = [];
  let envelope = {version: 1, root_public_key: userPublicKey, wraps: []};
  const identity = {
    kind: 'webcrypto-ed25519',
    userPublicKey,
    publicKey: Uint8Array.from(userPublicKey.match(/../g), byte => Number.parseInt(byte, 16)),
    locked: false,
    calls,
    get envelope() { return envelope; },
    get kept() { return envelope.wraps.some(wrap => wrap.kind === 'passkey' || wrap.kind === 'recovery'); },
    async sign() { return new Uint8Array(64); },
    async signRecord() { throw new Error('a fake identity signs nothing a host keeps'); },
    async keepWithPasskey(options = {}) {
      calls.push(['keepWithPasskey', options]);
      envelope = {...envelope, wraps: [...envelope.wraps,
        {kind: 'passkey', credential_id: 'AA', prf_salt: 'AA', rp_id: options.rpId ?? 'example.com'}]};
      return envelope;
    },
    async createRecoveryKey() {
      calls.push(['createRecoveryKey']);
      envelope = {...envelope, wraps: [...envelope.wraps.filter(wrap => wrap.kind !== 'recovery'),
        {kind: 'recovery', salt: 'AA', nonce: 'AA', ciphertext: 'AA'}]};
      return 'RECOVERY-KEY';
    },
    lock() { identity.locked = true; },
  };
  return identity;
}
