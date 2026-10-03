// A WebAuthn `navigator.credentials` double for node tests: passkeys with
// the PRF extension, evaluated as HMAC-SHA256 of a per-credential secret
// (as CTAP2 hmac-secret does). `prfAtCreate: false` models authenticators
// that report PRF only on assertion; `prf: false` ones without it.

const toBytes = value => ArrayBuffer.isView(value)
  ? new Uint8Array(value.buffer, value.byteOffset, value.byteLength)
  : new Uint8Array(value);
const base64url = bytes => Buffer.from(bytes).toString('base64url');

export function fakeAuthenticator({prf = true, prfAtCreate = true, seed = 0} = {}) {
  const passkeys = new Map();
  let created = 0;
  const log = [];
  async function evaluate(secret, salt) {
    const key = await crypto.subtle.importKey('raw', secret, {name: 'HMAC', hash: 'SHA-256'}, false, ['sign']);
    return new Uint8Array(await crypto.subtle.sign('HMAC', key, toBytes(salt)));
  }
  const credential = (id, results, enabled) => ({
    type: 'public-key',
    rawId: id.buffer.slice(0),
    getClientExtensionResults: () => (prf ? {prf: {...(enabled === undefined ? {} : {enabled}), ...(results ? {results} : {})}} : {}),
  });
  return {
    log,
    passkeys,
    /** Forget a passkey, as if the person deleted it. */
    remove: id => passkeys.delete(base64url(id)),
    async create({publicKey}) {
      log.push({op: 'create', rpId: publicKey.rp.id});
      const id = new Uint8Array(16).fill(seed).map((byte, index) => byte ^ index ^ (created * 37));
      created += 1;
      const secret = new Uint8Array(32).map((_, index) => (seed * 13 + created * 7 + index) & 0xff);
      passkeys.set(base64url(id), {id, secret, rpId: publicKey.rp.id});
      const salt = publicKey.extensions?.prf?.eval?.first;
      const results = prf && prfAtCreate && salt ? {first: (await evaluate(secret, salt)).buffer} : undefined;
      return credential(id, results, prf ? true : undefined);
    },
    async get({publicKey}) {
      log.push({op: 'get', rpId: publicKey.rpId});
      const allowed = publicKey.allowCredentials.map(entry => base64url(toBytes(entry.id)));
      const key = allowed.map(id => passkeys.get(id)).find(passkey => passkey?.rpId === publicKey.rpId);
      if (!key) throw Object.assign(new Error('no passkey'), {name: 'NotAllowedError'});
      const salt = publicKey.extensions?.prf?.evalByCredential?.[base64url(key.id)]?.first;
      const results = prf && salt ? {first: (await evaluate(key.secret, salt)).buffer} : undefined;
      return credential(key.id, results);
    },
  };
}
