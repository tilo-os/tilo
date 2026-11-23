/**
 * An unlocked identity with the calls the page makes on one, recording them:
 * a stand-in where the real identity (which needs the wasm runtime and a
 * `WebCrypto` device key key) is more than a page test needs.
 *
 * `calls` records what was asked.
 */
export function fakeIdentity({userPublicKey = 'ab'.repeat(32)} = {}) {
  const calls = [];
  const identity = {
    kind: 'webcrypto-ed25519',
    userPublicKey,
    publicKey: Uint8Array.from(userPublicKey.match(/../g), byte => Number.parseInt(byte, 16)),
    locked: false,
    calls,
    async sign() { return new Uint8Array(64); },
    async signRecord() { throw new Error('a fake identity signs nothing a host keeps'); },
    lock() { identity.locked = true; },
  };
  return identity;
}
