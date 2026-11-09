/**
 * A host's login codes and the person's stand-in approver, as the page asks
 * for them (`tilo-relay-protocol`'s `login_code`: routes, bodies and
 * answers).
 *
 * - `create(link)` makes a code: naming this tab's pairing link (setting up
 *   through an agent), or nothing (`null`: asking to be signed in by a
 *   device of the person). It resolves to `{code, expiresAt, follow()}`,
 *   where `follow()` asks where the code is (the host's `Polled`), with
 *   the secret only this tab got.
 * - `standIn(body)` asks the person's stand-in on this host; `body` is
 *   JSON text, since its `signed_at_ns` is a nanosecond time a JavaScript
 *   number would round (`standInBody`).
 */

/** Where a host answers, below its origin. */
export const API_PREFIX = '/peer/v1';
export const CREATE_ROUTE = `${API_PREFIX}/login-code`;
export const FOLLOW_ROUTE = `${API_PREFIX}/login-code/follow`;
export const STAND_IN_ROUTE = `${API_PREFIX}/stand-in`;

/** A host refused, or could not be reached: `status` is its HTTP status (0 unreached). */
export class LoginCodeError extends Error {
  constructor(status, message) {
    super(message);
    this.name = 'LoginCodeError';
    this.status = status;
    this.code = status === 403 ? 'not-a-device' : status === 404 ? 'unsupported' : 'unreachable';
  }
}

const hex = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
const unhex = text => Uint8Array.from(text.match(/../g) ?? [], pair => parseInt(pair, 16));

/** What the root signs to ask its stand-in for `op` (`key`, `retire`):
 * `tilo-stand-in-v1\0`, the op, `\0`, the person's root, the time (u64 LE). */
export function standInMessage(op, user, signedAtNs) {
  const head = new TextEncoder().encode(`tilo-stand-in-v1\0${op}\0`);
  const message = new Uint8Array(head.length + 32 + 8);
  message.set(head);
  message.set(unhex(user), head.length);
  new DataView(message.buffer).setBigUint64(head.length + 32, BigInt(signedAtNs), true);
  return message;
}

/** The body asking the stand-in for `op` (`key`, `retire`), signed by
 * `identity`'s root at `nowMs`: JSON text with the exact nanoseconds. */
export async function standInBody(op, identity, nowMs) {
  const user = identity.userPublicKey;
  const at = BigInt(Math.floor(nowMs)) * 1_000_000n;
  const signature = hex(await identity.sign(standInMessage(op, user, at)));
  return `{"op":${JSON.stringify(op)},"user":${JSON.stringify(user)},"signed_at_ns":${at},"signature":${JSON.stringify(signature)}}`;
}

/** The login codes and stand-in of the host at `origin`. */
export function loginCodes(origin, fetcher = (...args) => globalThis.fetch(...args)) {
  const post = async (route, body) => {
    let response;
    try {
      response = await fetcher(new URL(route, origin).href, {method: 'POST', headers: {'content-type': 'application/json'},
        body: typeof body === 'string' ? body : JSON.stringify(body), credentials: 'omit', cache: 'no-store'});
    } catch (error) {
      throw new LoginCodeError(0, `The host could not be reached: ${error?.message ?? error}`);
    }
    if (!response.ok) throw new LoginCodeError(response.status, `The host answered ${response.status}`);
    return response.status === 204 ? null : response.json();
  };
  return {
    async create(link = null) {
      const made = await post(CREATE_ROUTE, link ? {link} : {});
      return {code: made.code, expiresAt: made.expires_at_ms, follow: () => post(FOLLOW_ROUTE, {follow: made.follow})};
    },
    standIn: body => post(STAND_IN_ROUTE, body),
  };
}
