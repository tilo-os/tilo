// Discord's end-to-end encryption for a call: DAVE protocol version 1 over MLS (RFC 9420,
// ciphersuite 2), as the voice gateway and its whitepaper define it.
//
// The gateway is the MLS delivery service: it hands out its own credential as the group's one
// external sender (op 25), and proposes adding and removing the people in the call (op 27).
// This side keeps one group for the call, commits those proposals (op 28), joins when welcomed
// (op 30), and says it is ready for each transition (op 23) until the gateway executes it
// (op 22). Media frames are encrypted per sender under a key derived from the group's MLS
// export ("Discord Secure Frames v0", the sender's user id little-endian, 16 bytes), ratcheted
// by the generation the frame's nonce names.
//
// Opcodes 21 to 31 and the frame layout follow https://daveprotocol.com: server-to-client
// binary messages are a two-byte sequence number, the opcode, then the payload; a client
// sends the opcode then the payload. Opus frames are encrypted whole, so a DAVE frame is the
// ciphertext, the eight-byte truncated AES-GCM tag, the ULEB128 nonce, no unencrypted ranges,
// the supplemental size and the 0xFAFA marker.
//
// The MLS work is the vendored ts-mls (vendor/PROVENANCE.md). MLS and media crypto are
// WebCrypto and node:crypto, so nothing CPU-bound runs in the interpreter.

import { getCiphersuiteImpl } from "../vendor/ts-mls/crypto/getCiphersuiteImpl.js";
import { getCiphersuiteFromName } from "../vendor/ts-mls/crypto/ciphersuite.js";
import { deriveTreeSecret } from "../vendor/ts-mls/crypto/kdf.js";
import { makeHashImpl } from "../vendor/ts-mls/crypto/implementation/default/makeHashImpl.js";
import { makeHpke } from "../vendor/ts-mls/crypto/implementation/default/makeHpke.js";
import { makeKdf, makeKdfImpl } from "../vendor/ts-mls/crypto/implementation/default/makeKdfImpl.js";
import { defaultRng } from "../vendor/ts-mls/crypto/implementation/default/rng.js";
import { generateKeyPackage } from "../vendor/ts-mls/keyPackage.js";
import { createGroup, getGroupMembers, joinGroup } from "../vendor/ts-mls/clientState.js";
import { createCommit } from "../vendor/ts-mls/createCommit.js";
import { processPublicMessage } from "../vendor/ts-mls/processMessages.js";
import { mlsExporter } from "../vendor/ts-mls/keySchedule.js";
import { emptyPskIndex } from "../vendor/ts-mls/pskIndex.js";
import { encodeExternalSender } from "../vendor/ts-mls/externalSender.js";
import { decodeMlsMessage, encodeMlsMessage } from "../vendor/ts-mls/message.js";
import { decodeWelcome, encodeWelcome } from "../vendor/ts-mls/welcome.js";
import { determineLength } from "../vendor/ts-mls/codec/variableLength.js";
import { defaultClientConfig } from "../vendor/ts-mls/clientConfig.js";

/// The most DAVE protocol version this build speaks.
export const DAVE_VERSION = 1;
/// The ciphersuite DAVE protocol version 1 fixes.
export const SUITE = "MLS_128_DHKEMP256_AES128GCM_SHA256_P256";
/// The MLS export label per-sender media keys come from.
export const MEDIA_LABEL = "Discord Secure Frames v0";
/// Opus silence, which passes through unencrypted.
export const SILENCE = [0xf8, 0xff, 0xfe];
/// The two bytes that end every DAVE frame.
const MARKER = [0xfa, 0xfa];
/// The truncated AES-GCM authentication tag: eight bytes.
const TAG_BYTES = 8;
/// The smallest supplemental section: tag, size byte and marker.
const MIN_SUPPLEMENTAL = TAG_BYTES + 1 + 2;
/// How long a previous epoch's keys and a passthrough window live after a transition.
const TRANSITION_MS = 10_000;
/// A key never ratchets further than this ahead of the last generation seen.
const MAX_GENERATION_GAP = 250;
/// How many missing nonces are remembered for out-of-order frames.
const MAX_MISSING = 1_000;
/// No key package lifetime: DAVE's key packages span all time.
const FOREVER = { notBefore: 0n, notAfter: 2n ** 64n - 1n };
/// MLS's basic credential type.
const BASIC = 1;

const P256 = { name: "ECDSA", namedCurve: "P-256" };

/// The P-256 signature provider: ECDSA with SHA-256 over WebCrypto, in the DER form MLS signs
/// with. ts-mls would otherwise import its own pure-JS curve code, which is slow when the
/// interpreter runs it and unnecessary when the runtime has WebCrypto.
export const p256 = {
  async sign(signKey, message) {
    const key = await crypto.subtle.importKey("pkcs8", signKey, P256, false, ["sign"]);
    const raw = new Uint8Array(
      await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, key, message),
    );
    return derEncode(raw);
  },
  async verify(publicKey, message, signature) {
    const raw = derDecode(signature, 32);
    if (!raw) return false;
    const key = await crypto.subtle.importKey("raw", publicKey, P256, false, ["verify"]);
    return crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, key, raw, message);
  },
  async keygen() {
    const pair = await crypto.subtle.generateKey(P256, true, ["sign", "verify"]);
    const publicKey = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
    const signKey = new Uint8Array(await crypto.subtle.exportKey("pkcs8", pair.privateKey));
    return { signKey, publicKey };
  },
};

/// The ciphersuite's pieces: WebCrypto for the hash, KDF and HPKE, the P-256 provider above
/// for signatures. This is ts-mls's own default provider without its optional pure-JS
/// signature dependency.
export const daveCryptoProvider = {
  async getCiphersuiteImpl(cs) {
    return {
      kdf: makeKdfImpl(makeKdf(cs.hpke.kdf)),
      hash: makeHashImpl(crypto.subtle, cs.hash),
      signature: p256,
      hpke: await makeHpke(cs.hpke),
      rng: defaultRng,
      name: cs.name,
    };
  },
};

/// A DER `SEQUENCE { r INTEGER, s INTEGER }` from the raw r‖s WebCrypto signs with.
export function derEncode(raw) {
  const half = raw.length / 2;
  const r = trim(raw.subarray(0, half));
  const s = trim(raw.subarray(half));
  const body = Buffer.concat([
    Buffer.from([0x02, r.length]),
    r,
    Buffer.from([0x02, s.length]),
    s,
  ]);
  return Buffer.concat([Buffer.from([0x30, body.length]), body]);
}

/// The raw r‖s of a DER ECDSA signature, or null when it is not one of `width`.
export function derDecode(der, width) {
  if (der.length < 8 || der[0] !== 0x30 || der[1] !== der.length - 2) return null;
  let at = 2;
  const parts = [];
  for (let n = 0; n < 2; n += 1) {
    if (der[at] !== 0x02 || at + 2 > der.length) return null;
    const length = der[at + 1];
    at += 2;
    if (at + length > der.length) return null;
    let value = der.subarray(at, at + length);
    at += length;
    while (value.length > width && value[0] === 0) value = value.subarray(1);
    if (value.length > width) return null;
    const padded = Buffer.alloc(width);
    padded.set(value, width - value.length);
    parts.push(padded);
  }
  return Buffer.concat(parts);
}

function trim(bytes) {
  let at = 0;
  while (at < bytes.length - 1 && bytes[at] === 0) at += 1;
  const value = bytes.subarray(at);
  if (value[0] & 0x80) return Buffer.concat([Buffer.from([0]), value]);
  return value;
}

/// An unsigned LEB128 integer.
export function uleb(value) {
  const bytes = [];
  let left = value >>> 0;
  while (left >= 0x80) {
    bytes.push(0x80 | (left & 0x7f));
    left >>>= 7;
  }
  bytes.push(left);
  return Buffer.from(bytes);
}

/// The value and the offset just past the ULEB128 integer at `at` in `bytes`, or null.
export function readUleb(bytes, at) {
  let value = 0;
  let shift = 0;
  for (let i = at; i < bytes.length && i < at + 5; i += 1) {
    const byte = bytes[i];
    value |= (byte & 0x7f) << shift;
    if ((byte & 0x80) === 0) return { value: value >>> 0, at: i + 1 };
    shift += 7;
  }
  return null;
}

/// The per-sender key ratchet: MLS's tree secret derivation by generation, keys cached for a
/// short while so a late frame from a just-superseded generation still opens.
export class Ratchet {
  constructor(secret, kdf, now = Date.now) {
    this.next = Buffer.from(secret);
    this.kdf = kdf;
    this.now = now;
    this.generation = 0;
    this.keys = new Map();
    this.newest = -1;
    this.missing = new Set();
  }

  /// The AES-128-GCM key and 12-byte nonce of `generation`, or null when it is too far ahead.
  async get(generation) {
    if (generation < this.generation && this.keys.has(generation)) {
      return this.keys.get(generation);
    }
    if (generation > this.generation + MAX_GENERATION_GAP) return null;
    while (this.generation <= generation) {
      const gen = this.generation;
      const secret = await deriveTreeSecret(this.next, "key", gen, 16, this.kdf);
      const key = await crypto.subtle.importKey("raw", secret, { name: "AES-GCM" }, false, [
        "encrypt",
        "decrypt",
      ]);
      const nonce = await deriveTreeSecret(this.next, "nonce", gen, 12, this.kdf);
      this.next = await deriveTreeSecret(this.next, "secret", gen, 32, this.kdf);
      this.#prune();
      this.keys.set(gen, { key, nonce, at: this.now() });
      this.generation += 1;
    }
    return this.keys.get(generation) ?? null;
  }

  #prune() {
    const before = this.now() - TRANSITION_MS;
    for (const [generation, entry] of this.keys) {
      if (entry.at < before) this.keys.delete(generation);
    }
  }

  /// Whether a frame's nonce is one this ratchet has neither seen nor skipped, so a key and
  /// nonce pair is never used twice.
  accept(nonce) {
    if (this.newest < 0) return true;
    const wrapped = (nonce >>> 24) * 2 ** 24 + (nonce & 0xffffff);
    return wrapped > this.newest || this.missing.has(wrapped);
  }

  /// Keep the nonce as seen, filling in the ones between it and the newest.
  seen(nonce) {
    const wrapped = (nonce >>> 24) * 2 ** 24 + (nonce & 0xffffff);
    if (this.newest < 0 || wrapped > this.newest) {
      const oldest = Math.max(this.newest + 1, wrapped - MAX_MISSING);
      for (let at = oldest; at < wrapped; at += 1) this.missing.add(at);
      while (this.missing.size > MAX_MISSING) {
        this.missing.delete(this.missing.values().next().value);
      }
      this.newest = wrapped;
    } else {
      this.missing.delete(wrapped);
    }
  }
}

/// One DAVE session for one voice call.
///
/// `send(opcode, payload)` writes a client frame: a Buffer is binary, anything else JSON.
/// `ready(transitionId)` says this side is ready for a transition. Every MLS message the
/// gateway sends goes through one of the `process*` methods, which throw on a message they
/// cannot use so the caller can flag it (op 31) and rebuild the group.
export class DaveSession {
  constructor({ userId, channelId, send = () => {}, ready = () => {}, log = () => {} }) {
    this.userId = String(userId);
    this.channelId = String(channelId);
    this.send = send;
    this.ready = ready;
    this.log = log;
    this.version = 0;
    /// Whether the call expects end-to-end encrypted media from this side at all.
    this.wants = false;
    /// The media protocol in use: 0 before the first transition, 1 once an epoch is executed.
    this.protocol = 0;
    this.external = null;
    this.implementation = null;
    this.state = null;
    this.keyPackage = null;
    this.staged = null;
    this.sent = new Map();
    this.transitions = new Map();
    this.active = null;
    this.pending = null;
    this.retiring = [];
    this.senderNonce = 0;
    this.passthroughUntil = 0;
    this.announced = new Set();
  }

  /// The voice server's session description: the DAVE protocol version the call speaks.
  async setVersion(version) {
    this.version = Number(version) || 0;
    this.wants = this.version > 0;
    if (this.version <= 0) {
      this.#dropMedia();
      this.protocol = 0;
      return;
    }
    if (this.external && !this.#established()) await this.#prepare();
  }

  /// The gateway's external sender (op 25): the credential and key its proposals are signed
  /// with. The group is created as soon as both this and a non-zero protocol version are known.
  async setExternalSender(payload) {
    this.external = parseExternalSender(payload);
    if (this.#established()) {
      // A gateway that changes its external sender after the group was established is not one
      // this build can follow; the call keeps running on the group it has.
      this.log("the voice server named another external sender after the group was established");
      return;
    }
    this.state = null;
    this.staged = null;
    this.sent.clear();
    this.transitions.clear();
    this.announced.clear();
    if (this.version > 0) await this.#prepare();
  }

  /// The key package to send (op 26), or null while the group context is not known yet.
  keyPackageBytes() {
    if (!this.keyPackage) return null;
    return Buffer.from(
      encodeMlsMessage({
        version: "mls10",
        wireformat: "mls_key_package",
        keyPackage: this.keyPackage.publicPackage,
      }),
    );
  }

  /// The gateway announces a new group or a version change (op 24). An `epoch` of 1 means the
  /// group is (re)created: everything local is dropped and a new key package is sent.
  async prepareEpoch({ version, epoch, transitionId }) {
    const next = Number(version) || this.version;
    this.wants = next > 0;
    if (Number(epoch) === 1) {
      this.version = next;
      this.#dropMedia();
      this.protocol = 0;
      this.state = null;
      this.keyPackage = null;
      this.staged = null;
      this.sent.clear();
      this.transitions.clear();
      this.announced.clear();
      if (this.external && this.version > 0) await this.#prepare();
      return;
    }
    this.version = next;
    if (transitionId !== undefined) this.#announce(transitionId);
  }

  /// Proposals from the gateway (op 27): append them, then commit and welcome (op 28). The
  /// answer is `{commit, welcome}` as bytes to send, or null when nothing is queued.
  async processProposals(payload) {
    const impl = await this.#impl();
    if (!this.state) throw new Error("the group is not known yet");
    const operation = payload[0];
    const body = vectorAt(payload, 1);
    if (operation === 1) {
      // Revoked proposals leave the queue. The gateway only revokes proposals it queued, and
      // none of this build's own commits can be revoked, so a queued revocation only needs
      // the refs to leave this side's queue; ts-mls keeps them until a commit covers them, so
      // a revocation before a commit is answered by committing what is still queued.
      const revoked = new Set();
      for (let at = 0; at < body.length; ) {
        const ref = determineLength(body, at);
        if (!ref) throw new Error("a revoked proposal reference did not decode");
        revoked.add(body.subarray(at + ref.lengthFieldSize, at + ref.lengthFieldSize + ref.length).toString("base64"));
        at += ref.lengthFieldSize + ref.length;
      }
      return this.#commitQueued(revoked);
    }
    if (operation !== 0) throw new Error(`an unknown proposals operation (${operation})`);
    let offset = 0;
    let state = this.state;
    let invalid = null;
    while (offset < body.length) {
      const decoded = decodeMlsMessage(body, offset);
      if (!decoded) throw new Error("a proposal message did not decode");
      const [message, read] = decoded;
      offset += read;
      if (message.wireformat !== "mls_public_message") {
        throw new Error("a proposal was not a public MLS message");
      }
      const content = message.publicMessage.content;
      if (content.sender?.senderType !== "external" || content.sender.senderIndex !== 0) {
        throw new Error("a proposal was not from the voice server");
      }
      if (content.contentType !== "proposal") {
        throw new Error("a proposal message carried no proposal");
      }
      const result = await processPublicMessage(state, message.publicMessage, emptyPskIndex, impl, (heard) => {
        if (heard.kind !== "proposal") return "reject";
        const type = heard.proposal?.proposal?.proposalType;
        if (type !== "add" && type !== "remove") {
          invalid = `a ${type} proposal is not one DAVE takes`;
          return "reject";
        }
        return "accept";
      });
      state = result.newState;
    }
    if (invalid) throw new Error(invalid);
    this.staged = state;
    this.state = state;
    return this.#commitQueued(new Set());
  }

  /// Commit the queued proposals, with `revoked` refs dropped from the queue first, and answer
  /// what to send (op 28) or null when nothing is queued.
  async #commitQueued(revoked) {
    const impl = await this.#impl();
    let state = this.staged ?? this.state;
    if (revoked.size > 0) {
      const kept = Object.fromEntries(
        Object.entries(state.unappliedProposals).filter(([ref]) => !revoked.has(ref)),
      );
      state = { ...state, unappliedProposals: kept };
      this.state = state;
      this.staged = state;
    }
    if (Object.keys(state.unappliedProposals).length === 0) return null;
    const committed = await createCommit(
      { state, cipherSuite: impl },
      { wireAsPublicMessage: true, ratchetTreeExtension: true },
    );
    this.sent.set(hex(encodeMlsMessage(committed.commit)), committed.newState);
    const commit = Buffer.from(encodeMlsMessage(committed.commit));
    const welcome = committed.welcome ? Buffer.from(encodeWelcome(committed.welcome)) : null;
    return { commit, welcome };
  }

  /// The gateway announces the commit that won the epoch (op 29). Answers whether this side is
  /// ready for `transitionId`.
  async processCommit(transitionId, payload) {
    const impl = await this.#impl();
    const decoded = decodeMlsMessage(payload, 0);
    if (!decoded) throw new Error("the commit did not decode");
    const [message, read] = decoded;
    if (message.wireformat !== "mls_public_message") {
      throw new Error("the commit was not a public MLS message");
    }
    const ours = this.sent.get(hex(payload.subarray(0, read)));
    if (ours) {
      this.state = ours;
    } else {
      const staged = this.staged ?? this.state;
      if (!staged) throw new Error("the commit came before the group was known");
      if (message.publicMessage.content?.sender?.senderType !== "member") {
        throw new Error("the commit was not from a group member");
      }
      this.#commitCovers(staged, message.publicMessage);
      const result = await processPublicMessage(
        staged,
        message.publicMessage,
        emptyPskIndex,
        impl,
        () => "accept",
      );
      this.state = result.newState;
    }
    this.staged = null;
    this.sent.clear();
    await this.#transition(transitionId);
    return true;
  }

  /// A welcome for this member (op 30). Answers whether this side is ready for `transitionId`.
  async processWelcome(transitionId, payload) {
    const impl = await this.#impl();
    if (!this.keyPackage) throw new Error("no key package was announced for this welcome");
    const decoded = decodeWelcome(payload, 0);
    if (!decoded) throw new Error("the welcome did not decode");
    const [welcome] = decoded;
    const joined = await joinGroup(
      welcome,
      this.keyPackage.publicPackage,
      this.keyPackage.privatePackage,
      emptyPskIndex,
      impl,
      undefined,
      undefined,
      defaultClientConfig,
    );
    this.#sameExternalSender(joined);
    this.state = joined;
    this.keyPackage = null;
    this.staged = null;
    this.sent.clear();
    await this.#transition(transitionId);
    return true;
  }

  /// A transition is prepared (op 21): a downgrade to protocol version 0 waits for execute;
  /// transition 0 is initialization and executes at once.
  async prepareTransition({ version, transitionId }) {
    const id = Number(transitionId) || 0;
    if (id === 0) {
      await this.executeTransition(0);
      return;
    }
    if (Number(version) === 0) this.transitions.set(id, { kind: "downgrade" });
    this.#announce(id);
  }

  /// A prepared transition is executed (op 22): media switches to what it prepared, or to the
  /// protocol version named by a version change (op 24).
  async executeTransition(transitionId) {
    const id = Number(transitionId) || 0;
    const entry = this.transitions.get(id);
    if (entry) {
      if (entry.kind === "epoch") {
        await this.#activate(entry);
      } else {
        this.#dropMedia();
        this.protocol = 0;
        // A downgrade leaves the call clear: nothing more is asked to be encrypted.
        this.wants = false;
      }
      this.transitions.delete(id);
    } else if (this.pending && (this.wants || id === this.pending.id)) {
      // Any executed transition while the call wants encrypted media begins on the epoch
      // prepared for it, whichever id the gateway names it by.
      const pending = this.pending;
      this.pending = null;
      await this.#activate(pending);
    }
    this.passthroughUntil = Date.now() + TRANSITION_MS;
  }

  /// A commit or welcome could not be used: flag it (op 31) and rebuild the group.
  async recover(transitionId) {
    this.state = null;
    this.keyPackage = null;
    this.staged = null;
    this.sent.clear();
    this.#dropMedia();
    this.protocol = 0;
    if (transitionId !== undefined) this.send(31, { op: 31, d: { transition_id: transitionId } });
    if (this.external && this.version > 0) await this.#prepare();
  }

  /// Whether the call expects end-to-end encrypted media from this side and has none yet.
  needsKeys() {
    return this.wants && !this.sending();
  }

  /// Whether media is sent under the protocol frame format right now.
  sending() {
    return this.protocol === 1 && this.active !== null;
  }

  /// How many members the current group has; 0 when there is none.
  members() {
    return this.state ? getGroupMembers(this.state).length : 0;
  }

  /// One Opus frame as it goes on the wire: a DAVE frame once the call speaks the protocol;
  /// silence, and anything before the first transition, as it is.
  async encrypt(opus) {
    const frame = Buffer.from(opus);
    if (!this.sending() || isSilence(frame)) return frame;
    const ratchet = this.active.get(this.userId);
    if (!ratchet) return frame;
    this.senderNonce = (this.senderNonce + 1) >>> 0;
    if (this.senderNonce === 0) throw new Error("the DAVE nonce ran out");
    const keys = await ratchet.get(this.senderNonce >>> 24);
    if (!keys) throw new Error("the DAVE key ratchet has no key for this frame");
    const nonce = Buffer.alloc(12);
    nonce.writeUInt32LE(this.senderNonce, 8);
    // An Opus frame is encrypted whole: there are no unencrypted ranges to authenticate.
    // WebCrypto appends the (64-bit) tag to the ciphertext, which is the frame's order.
    const sealed = Buffer.from(
      await crypto.subtle.encrypt(
        { name: "AES-GCM", iv: nonce, tagLength: TAG_BYTES * 8, additionalData: new Uint8Array(0) },
        keys.key,
        frame,
      ),
    );
    const ciphertext = sealed.subarray(0, sealed.length - TAG_BYTES);
    const tag = sealed.subarray(sealed.length - TAG_BYTES);
    const counter = uleb(this.senderNonce);
    const size = MIN_SUPPLEMENTAL + counter.length;
    return Buffer.concat([
      ciphertext,
      tag,
      counter,
      Buffer.from([size]),
      Buffer.from(MARKER),
    ]);
  }

  /// One frame heard from `uid`: the Opus frame it holds, or null when it is not one this
  /// side can use.
  async decrypt(uid, frame) {
    const bytes = Buffer.from(frame);
    if (isSilence(bytes)) return bytes;
    const parsed = parseFrame(bytes);
    if (!parsed) {
      // Not a DAVE frame: media from before a transition, which still passes for a moment
      // after one; anything else in an encrypted call is dropped.
      if (this.active || this.pending) {
        return this.passthroughUntil > Date.now() ? bytes : null;
      }
      return bytes;
    }
    const keys = [];
    if (this.active) keys.push(this.active);
    if (this.pending) keys.push(this.pending.keys);
    for (const retiring of this.retiring) keys.push(retiring.keys);
    for (const ratchets of keys) {
      const ratchet = ratchets.get(String(uid));
      if (!ratchet) continue;
      if (!ratchet.accept(parsed.nonce)) continue;
      const pair = await ratchet.get(parsed.nonce >>> 24);
      if (!pair) continue;
      const opened = await openFrame(parsed, pair);
      if (opened === null) continue;
      ratchet.seen(parsed.nonce);
      return opened;
    }
    return null;
  }

  #established() {
    return Boolean(this.state && this.state.groupContext.epoch > 0n);
  }

  /// The group's one external sender is the one the gateway named.
  #sameExternalSender(state) {
    if (!this.external) throw new Error("the gateway named no external sender");
    const present = (state.groupContext.extensions ?? [])
      .filter((extension) => extension.extensionType === "external_senders")
      .map((extension) => Buffer.from(extension.extensionData));
    const expected = Buffer.from(encodeExternalSender(this.external));
    if (present.length !== 1 || !present[0].equals(expected)) {
      throw new Error("the group's external sender is not the voice server's");
    }
  }

  /// Whether a commit's proposal references are exactly the ones this side queued.
  #commitCovers(state, publicMessage) {
    const commit = publicMessage.content?.commit;
    if (!commit) throw new Error("the commit message carried no commit");
    const queued = new Set(Object.keys(state.unappliedProposals));
    const referenced = new Set();
    for (const entry of commit.proposals ?? []) {
      if (entry.proposalOrRefType !== "reference") {
        throw new Error("the commit included a proposal instead of referring to one");
      }
      const ref = Buffer.from(entry.reference).toString("base64");
      if (referenced.has(ref)) throw new Error("the commit referred to a proposal twice");
      referenced.add(ref);
    }
    if (queued.size !== referenced.size || [...queued].some((ref) => !referenced.has(ref))) {
      throw new Error("the commit did not cover the proposals it had to");
    }
  }

  /// A commit or welcome moved this side to a new epoch: prepare its media keys, to be used
  /// once the gateway executes the transition.
  async #transition(transitionId) {
    const id = Number(transitionId) || 0;
    const keys = await this.#mediaKeys();
    const entry = { id, kind: "epoch", keys };
    this.transitions.set(id, entry);
    if (this.pending) this.retiring.push({ keys: this.pending.keys, until: Date.now() + TRANSITION_MS });
    this.pending = entry;
    this.#announce(id);
  }

  async #activate(entry) {
    if (this.active) this.retiring.push({ keys: this.active, until: Date.now() + TRANSITION_MS });
    this.active = entry.keys;
    if (this.pending && this.pending.id === entry.id) this.pending = null;
    this.protocol = 1;
    this.senderNonce = 0;
    const before = Date.now();
    this.retiring = this.retiring.filter((old) => old.until > before);
  }

  /// The per-sender media keys of the current MLS state.
  async #mediaKeys() {
    const impl = await this.#impl();
    const ratchets = new Map();
    for (const leaf of getGroupMembers(this.state)) {
      const uid = credentialUserId(leaf.credential);
      if (!uid) continue;
      const base = await mlsExporter(
        this.state.keySchedule.exporterSecret,
        MEDIA_LABEL,
        userIdLeBytes(uid),
        16,
        impl,
      );
      ratchets.set(uid, new Ratchet(base, impl.kdf));
    }
    return ratchets;
  }

  #announce(transitionId) {
    const id = Number(transitionId) || 0;
    if (this.announced.has(id)) return;
    this.announced.add(id);
    this.ready(id);
  }

  #dropMedia() {
    this.active = null;
    this.pending = null;
    this.retiring = [];
    this.senderNonce = 0;
  }

  /// The pending group of this side alone, and a key package to be added through it.
  async #prepare() {
    if (!this.external || this.version <= 0) return;
    const impl = await this.#impl();
    this.keyPackage = await generateKeyPackage(
      { credentialType: "basic", identity: userIdBytes(this.userId) },
      {
        versions: ["mls10"],
        ciphersuites: [SUITE],
        extensions: [],
        proposals: [],
        credentials: ["basic"],
      },
      FOREVER,
      [],
      impl,
    );
    this.state = await createGroup(
      channelIdBytes(this.channelId),
      this.keyPackage.publicPackage,
      this.keyPackage.privatePackage,
      [{ extensionType: "external_senders", extensionData: encodeExternalSender(this.external) }],
      impl,
    );
    this.staged = null;
    this.sent.clear();
    this.announced.clear();
    const bytes = this.keyPackageBytes();
    if (bytes) this.send(26, bytes);
  }

  async #impl() {
    if (!this.implementation) {
      this.implementation = getCiphersuiteImpl(getCiphersuiteFromName(SUITE), daveCryptoProvider);
    }
    return this.implementation;
  }
}

/// The bytes of the MLS vector whose length starts at `at`.
function vectorAt(bytes, at) {
  const length = determineLength(bytes, at);
  if (!length) throw new Error("a vector length did not decode");
  const start = at + length.lengthFieldSize;
  return bytes.subarray(start, start + length.length);
}

/// A DAVE frame split into what a decryptor needs, or null when it is not one.
export function parseFrame(bytes) {
  if (bytes.length < MIN_SUPPLEMENTAL) return null;
  if (bytes[bytes.length - 2] !== MARKER[0] || bytes[bytes.length - 1] !== MARKER[1]) return null;
  const size = bytes[bytes.length - 3];
  if (size < MIN_SUPPLEMENTAL || size > bytes.length) return null;
  const start = bytes.length - size;
  const tag = bytes.subarray(start, start + TAG_BYTES);
  const read = readUleb(bytes, start + TAG_BYTES);
  if (!read) return null;
  const end = bytes.length - 3;
  const ranges = [];
  let at = read.at;
  while (at < end) {
    const offset = readUleb(bytes, at);
    if (!offset) return null;
    const length = readUleb(bytes, offset.at);
    if (!length) return null;
    at = length.at;
    ranges.push({ offset: offset.value, size: length.value });
  }
  let last = 0;
  for (const range of ranges) {
    if (range.offset < last || range.offset + range.size > start) return null;
    last = range.offset + range.size;
  }
  const unencrypted = ranges.map((range) => bytes.subarray(range.offset, range.offset + range.size));
  const ciphertext = [];
  let frameAt = 0;
  for (const range of ranges) {
    if (range.offset > frameAt) ciphertext.push(bytes.subarray(frameAt, range.offset));
    frameAt = range.offset + range.size;
  }
  if (frameAt < start) ciphertext.push(bytes.subarray(frameAt, start));
  return {
    tag,
    nonce: read.value,
    ranges,
    authenticated: Buffer.concat(unencrypted),
    ciphertext: Buffer.concat(ciphertext),
    media: start,
  };
}

/// The plaintext of a parsed DAVE frame under one key, or null when it does not open.
export async function openFrame(parsed, pair) {
  const nonce = Buffer.alloc(12);
  nonce.writeUInt32LE(parsed.nonce, 8);
  const sealed = Buffer.concat([parsed.ciphertext, parsed.tag]);
  let opened;
  try {
    opened = Buffer.from(
      await crypto.subtle.decrypt(
        {
          name: "AES-GCM",
          iv: nonce,
          tagLength: TAG_BYTES * 8,
          additionalData: parsed.authenticated,
        },
        pair.key,
        sealed,
      ),
    );
  } catch {
    return null;
  }
  // Put the unencrypted ranges back where they were in the media frame.
  const out = Buffer.alloc(parsed.media);
  let encryptedAt = 0;
  let plainAt = 0;
  let frameAt = 0;
  for (const range of parsed.ranges) {
    const length = range.offset - frameAt;
    opened.copy(out, frameAt, encryptedAt, encryptedAt + length);
    encryptedAt += length;
    parsed.authenticated.copy(out, range.offset, plainAt, plainAt + range.size);
    plainAt += range.size;
    frameAt = range.offset + range.size;
  }
  opened.copy(out, frameAt, encryptedAt, encryptedAt + (parsed.media - frameAt));
  return out;
}

function isSilence(frame) {
  return frame.length === SILENCE.length && SILENCE.every((byte, at) => frame[at] === byte);
}

/// A user id's eight big-endian bytes, as an MLS credential identity.
export function userIdBytes(userId) {
  const bytes = Buffer.alloc(8);
  bytes.writeBigUInt64BE(BigInt(String(userId)));
  return bytes;
}

/// A user id's eight little-endian bytes, as a media key export context.
export function userIdLeBytes(userId) {
  const bytes = Buffer.alloc(8);
  bytes.writeBigUInt64LE(BigInt(String(userId)));
  return bytes;
}

/// The group id of a call: its channel id, big-endian.
export function channelIdBytes(channelId) {
  const bytes = Buffer.alloc(8);
  bytes.writeBigUInt64BE(BigInt(String(channelId)));
  return bytes;
}

/// The user id a basic credential holds, or null for another credential type.
export function credentialUserId(credential) {
  if (credential?.credentialType !== "basic" || credential.identity?.length !== 8) return null;
  return Buffer.from(credential.identity).readBigUInt64BE().toString();
}

/// The gateway's external sender (op 25 payload): a signature public key, then a basic
/// credential. The group extension it builds is the RFC's one-element vector.
export function parseExternalSender(payload) {
  const key = determineLength(payload, 0);
  if (!key) throw new Error("the external sender's key did not decode");
  let at = key.lengthFieldSize + key.length;
  if (at > payload.length) throw new Error("the external sender's key did not decode");
  const signaturePublicKey = Buffer.from(payload.subarray(key.lengthFieldSize, at));
  if (payload.length < at + 2) throw new Error("the external sender's credential did not decode");
  if (payload.readUInt16BE(at) !== BASIC) {
    throw new Error("the external sender's credential is not basic");
  }
  at += 2;
  const identity = determineLength(payload, at);
  if (!identity) throw new Error("the external sender's identity did not decode");
  const start = at + identity.lengthFieldSize;
  return {
    signaturePublicKey,
    credential: {
      credentialType: "basic",
      identity: Buffer.from(payload.subarray(start, start + identity.length)),
    },
  };
}

function hex(bytes) {
  return Buffer.from(bytes).toString("hex");
}
