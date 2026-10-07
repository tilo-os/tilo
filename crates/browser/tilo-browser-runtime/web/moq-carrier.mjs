/**
 * The browser side of the Tilo MoQ carrier: `moq-lite` over WebTransport,
 * opened by the host admission preface (the host side and the wire contract
 * are documented in tilo-moq's `carrier` module).
 *
 * 1. `new WebTransport(url, {protocols: [MOQ_PROTOCOL]})`; the URL is routing
 *    only and never carries a grant (URLs reach logs and history, and
 *    browsers cannot attach headers to a WebTransport CONNECT).
 * 2. On the first bidirectional stream: `version u8 = 1 | length u16 BE |
 *    grant`, then read one status byte: 0 admitted, 1 denied.
 * 3. After ADMITTED, hand the transport to `@moq/net` (`Connection.connect`
 *    with `transport`). The admission stream stays open: any later byte (2 =
 *    revoked) or its end means the host revoked the session, which is then
 *    closed from this side first.
 */
export const MOQ_PROTOCOL = 'moq-lite-06';
export const PREFACE_VERSION = 1;
export const MAX_GRANT_BYTES = 8192;
export const STATUS_ADMITTED = 0;
export const STATUS_DENIED = 1;
export const STATUS_REVOKED = 2;
const CLOSE_DENIED = 0x7401;
const CLOSE_REVOKED = 0x7402;

/** An explicit refusal from the authenticated carrier, rather than a lost path. */
export class MediaAdmissionError extends Error {
  constructor() {super('media admission denied'); this.name = 'MediaAdmissionError';}
}

/** The admission preface bytes for `grant`. */
export function encodePreface(grant) {
  if (!(grant instanceof Uint8Array) || grant.byteLength === 0 || grant.byteLength > MAX_GRANT_BYTES) {
    throw new TypeError(`grant must be 1..${MAX_GRANT_BYTES} bytes`);
  }
  const bytes = new Uint8Array(3 + grant.byteLength);
  bytes[0] = PREFACE_VERSION;
  bytes[1] = grant.byteLength >> 8;
  bytes[2] = grant.byteLength & 0xff;
  bytes.set(grant, 3);
  return bytes;
}

async function readByte(reader) {
  for (;;) {
    const {value, done} = await reader.read();
    if (done) return undefined;
    if (value?.byteLength) return value[0];
  }
}

/**
 * Open a WebTransport session to `url`, present `grant`, and resolve once the
 * host admitted it. Resolves `{transport, revoked, authorityLost, close}`: `transport` is the
 * WebTransport to give `@moq/net`, `revoked` settles when the host revokes the
 * session (the transport is closed by then). `authorityLost` resolves only
 * on an explicit authenticated refusal/revocation, never ordinary path loss.
 */
export async function openCarrier({url, grant, serverCertificateHashes, signal, WebTransport = globalThis.WebTransport}) {
  signal?.throwIfAborted?.();
  const preface = encodePreface(grant);
  if (typeof WebTransport !== 'function') throw new Error('WebTransport is unavailable');
  const options = {protocols: [MOQ_PROTOCOL], allowPooling: false, congestionControl: 'low-latency'};
  if (serverCertificateHashes) options.serverCertificateHashes = serverCertificateHashes;
  const transport = new WebTransport(url.href, options);
  let admissionRevoked = false;
  let authorityLostResolve;
  const authorityLost = new Promise(resolve => {authorityLostResolve = resolve;});
  const loseAuthority = () => {admissionRevoked = true; authorityLostResolve();};
  // The authenticated close code covers a revoked session whose control byte
  // was not delivered. Ordinary network failure carries no such evidence.
  void transport.closed?.then(info => {
    if (info?.closeCode === CLOSE_REVOKED || info?.closeCode === CLOSE_DENIED) loseAuthority();
  }, () => {});
  const close = () => { try { transport.close(); } catch {} };
  const onAbort = () => close();
  signal?.addEventListener?.('abort', onAbort, {once: true});
  try {
    await transport.ready;
    if (transport.protocol !== MOQ_PROTOCOL) throw new Error(`carrier did not negotiate ${MOQ_PROTOCOL}`);
    const stream = await transport.createBidirectionalStream();
    const writer = stream.writable.getWriter();
    await writer.write(preface);
    const reader = stream.readable.getReader();
    const status = await readByte(reader);
    if (status === STATUS_DENIED) throw new MediaAdmissionError();
    if (status !== STATUS_ADMITTED) throw new Error('media admission denied');
    signal?.throwIfAborted?.();
    const revoked = readByte(reader).catch(() => undefined).then(status => {
      if (status === STATUS_REVOKED) loseAuthority();
      close();
    });
    return {transport, revoked, authorityLost, get admissionRevoked() {return admissionRevoked;}, close};
  } catch (error) {
    close();
    throw error;
  } finally {
    signal?.removeEventListener?.('abort', onAbort);
  }
}

/**
 * The broadcast an origin currently resolves for `path`, waiting until one
 * does. Returns `{broadcast, request}`; closing `request` releases it.
 */
export async function requestBroadcast(origin, path, signal) {
  const request = origin.request(path);
  try {
    for (;;) {
      const broadcast = request.active.peek();
      if (broadcast) return {broadcast, request};
      signal?.throwIfAborted?.();
      await request.active.changed();
    }
  } catch (error) {
    request.close();
    throw error;
  }
}
