// RTP packets on a Discord voice server's UDP socket, and their transport encryption in Discord's
// `*_rtpsize` AEAD modes.
//
// In those modes the RTP header, its CSRCs and the 4-byte preamble of a header extension stay in
// the clear and are the AEAD's associated data; the extension's body and the Opus payload are
// encrypted, the 16-byte tag follows, and a 32-bit big-endian counter is appended in the clear.
// The counter is the nonce's first four bytes, the rest of the nonce is zero.
//
// AES-256-GCM is the mode this program speaks: Bun's cryptography carries it on every platform,
// and Discord always offers it. XChaCha20-Poly1305 is offered by the same servers but no
// JavaScript runtime here ships it; a server that offers nothing else is refused with that reason
// rather than a call that sends nothing.

import { createCipheriv, createDecipheriv } from "node:crypto";

/// RTP payload type Discord uses for Opus.
export const OPUS_PAYLOAD_TYPE = 0x78;
/// Samples per 20 ms Opus frame at 48 kHz: the RTP timestamp's step.
export const FRAME_SAMPLES = 960;
const HEADER = 12;
const NONCE_SUFFIX = 4;
const TAG_BYTES = 16;

export const AES_GCM = "aead_aes256_gcm_rtpsize";
export const XCHACHA = "aead_xchacha20_poly1305_rtpsize";

/// The mode to select from the ones a voice server offers, or why none here is usable. AES comes
/// first, as it does for Discord's own clients; a server that only offers XChaCha20-Poly1305 is
/// one this program cannot hear or speak in.
export function chooseMode(offered) {
  const list = [...(offered ?? [])];
  if (list.includes(AES_GCM)) return { mode: AES_GCM };
  if (list.includes(XCHACHA)) {
    return {
      reason:
        "the voice server offers only aead_xchacha20_poly1305_rtpsize, which this build's " +
        "cryptography does not carry",
    };
  }
  return { reason: "the voice server offers no encryption mode this bridge supports" };
}

/// The 12-byte AES-GCM nonce of `counter`: its big-endian bytes, then zeros.
function shortNonce(counter) {
  const nonce = Buffer.alloc(12);
  nonce.writeUInt32BE(counter >>> 0, 0);
  return nonce;
}

export class Cipher {
  /// The cipher for `mode` under `key` (32 bytes). Only [`AES_GCM`] is here; another mode throws
  /// with the reason a person can act on.
  constructor(mode, key) {
    if (mode !== AES_GCM) {
      throw new Error(`${mode} is not an encryption mode this build carries`);
    }
    if (!key || key.length !== 32) {
      throw new Error(`a voice key is 32 bytes, not ${key?.length ?? 0}`);
    }
    this.mode = mode;
    this.key = Buffer.from(key);
  }

  /// `body` sealed under `counter` with `aad`, its 16-byte tag appended.
  seal(counter, aad, body) {
    const cipher = createCipheriv("aes-256-gcm", this.key, shortNonce(counter));
    cipher.setAAD(aad);
    return Buffer.concat([cipher.update(body), cipher.final(), cipher.getAuthTag()]);
  }

  /// What [`seal`] sealed under `counter` with `aad`; `null` for another key, counter or
  /// associated data, or modified bytes.
  open(counter, aad, sealed) {
    if (sealed.length < TAG_BYTES) return null;
    const body = sealed.subarray(0, sealed.length - TAG_BYTES);
    const tag = sealed.subarray(sealed.length - TAG_BYTES);
    try {
      const decipher = createDecipheriv("aes-256-gcm", this.key, shortNonce(counter));
      decipher.setAAD(aad);
      decipher.setAuthTag(tag);
      return Buffer.concat([decipher.update(body), decipher.final()]);
    } catch {
      return null;
    }
  }
}

/// What the bridge sends: one Opus frame per packet, numbered on from a random start.
export class Sender {
  constructor(ssrc, start = crypto.getRandomValues(new Uint8Array(10))) {
    this.ssrc = ssrc >>> 0;
    this.sequence = (start[0] << 8) | start[1];
    this.timestamp =
      ((start[2] << 24) | (start[3] << 16) | (start[4] << 8) | start[5]) >>> 0;
    this.counter =
      ((start[6] << 24) | (start[7] << 16) | (start[8] << 8) | start[9]) >>> 0;
  }

  /// A frame that is not sent: its place in the timeline is still spent, so the receiver sees the
  /// sequence and timestamp advance without a packet and conceals the 20 ms it never got.
  skip() {
    this.sequence = (this.sequence + 1) & 0xffff;
    this.timestamp = (this.timestamp + FRAME_SAMPLES) >>> 0;
  }

  /// The next packet carrying `frame`, sealed with `cipher`, or `null` when it cannot be sealed.
  packet(cipher, frame) {
    const header = Buffer.alloc(HEADER);
    header[0] = 0x80;
    header[1] = OPUS_PAYLOAD_TYPE;
    header.writeUInt16BE(this.sequence, 2);
    header.writeUInt32BE(this.timestamp, 4);
    header.writeUInt32BE(this.ssrc, 8);
    const sealed = cipher.seal(this.counter, header, frame);
    const counter = Buffer.alloc(NONCE_SUFFIX);
    counter.writeUInt32BE(this.counter, 0);
    this.sequence = (this.sequence + 1) & 0xffff;
    this.timestamp = (this.timestamp + FRAME_SAMPLES) >>> 0;
    this.counter = (this.counter + 1) >>> 0;
    return Buffer.concat([header, sealed, counter]);
  }
}

/// Open an RTP packet from the voice server. `null` for RTCP, for anything that is not RTP
/// version 2 carrying Opus, and for what does not decrypt.
export function open(cipher, packet) {
  if (packet.length < HEADER + NONCE_SUFFIX) return null;
  const first = packet[0];
  const kind = packet[1];
  if (first >> 6 !== 2 || (kind & 0x7f) !== OPUS_PAYLOAD_TYPE) return null;
  const csrcs = first & 0x0f;
  const hasExtension = (first & 0x10) !== 0;
  let header = HEADER + csrcs * 4;
  let extensionWords = 0;
  if (hasExtension) {
    if (packet.length < header + 4) return null;
    extensionWords = packet.readUInt16BE(header + 2);
    header += 4;
  }
  const bodyEnd = packet.length - NONCE_SUFFIX;
  if (header > bodyEnd) return null;
  const aad = packet.subarray(0, header);
  const sealed = packet.subarray(header, bodyEnd);
  const counter = packet.readUInt32BE(bodyEnd);
  const plain = cipher.open(counter, aad, sealed);
  if (plain === null) return null;
  const skip = extensionWords * 4;
  if (skip > plain.length) return null;
  return {
    ssrc: packet.readUInt32BE(8),
    sequence: packet.readUInt16BE(2),
    timestamp: packet.readUInt32BE(4),
    frame: plain.subarray(skip),
  };
}
