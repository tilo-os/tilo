// What DAVE costs the bridge per 20 ms frame, measured on the real session: one group of two
// members, one side encrypting and the other decrypting 20 ms Opus frames.
//
// Run it on the runtime the app ships with, with the interpreter where that is how apps run:
//   bun bench/dave-cpu.js
//   BUN_JSC_useJIT=0 bun bench/dave-cpu.js

import { DaveSession, SUITE, daveCryptoProvider, p256, userIdBytes } from "../src/dave.js";
import { getCiphersuiteImpl } from "../vendor/ts-mls/crypto/getCiphersuiteImpl.js";
import { getCiphersuiteFromName } from "../vendor/ts-mls/crypto/ciphersuite.js";
import { protectExternalProposalPublic } from "../vendor/ts-mls/messageProtectionPublic.js";
import { encodeMlsMessage } from "../vendor/ts-mls/message.js";
import { lengthEncoder } from "../vendor/ts-mls/codec/variableLength.js";

/// The opus frames the benchmark says: 120 bytes is a typical 20 ms frame.
const FRAMES = 5_000;
const OPUS = new Uint8Array(120);
for (let at = 0; at < OPUS.length; at += 1) OPUS[at] = (at * 7) & 0xff;

function vector(bytes) {
  const [length, write] = lengthEncoder(bytes.length);
  const out = new Uint8Array(length + bytes.length);
  write(0, out.buffer);
  out.set(bytes, length);
  return Buffer.from(out);
}

const impl = await getCiphersuiteImpl(getCiphersuiteFromName(SUITE), daveCryptoProvider);

const gateKeys = await p256.keygen();
const gate = {
  signaturePublicKey: Buffer.from(gateKeys.publicKey),
  credential: { credentialType: "basic", identity: userIdBytes("1") },
};
const senderPayload = Buffer.concat([
  vector(gate.signaturePublicKey),
  Buffer.from([0, 1]),
  vector(gate.credential.identity),
]);

const make = (user) =>
  new DaveSession({ userId: user, channelId: "458", send: () => {}, ready: () => {}, log: () => {} });

const a = make("1001");
const b = make("1002");
for (const member of [a, b]) {
  await member.setVersion(1);
  await member.setExternalSender(senderPayload);
}

const proposal = await protectExternalProposalPublic(
  gateKeys.signKey,
  a.state.groupContext,
  new Uint8Array(),
  { proposalType: "add", add: { keyPackage: b.keyPackage.publicPackage } },
  { senderType: "external", senderIndex: 0 },
  impl,
);
const message = Buffer.from(
  encodeMlsMessage({ version: "mls10", wireformat: "mls_public_message", publicMessage: proposal.publicMessage }),
);
const committed = await a.processProposals(Buffer.concat([Buffer.from([0]), vector(message), message]));
await a.processCommit(1, committed.commit);
await b.processWelcome(1, committed.welcome);
await a.executeTransition(1);
await b.executeTransition(1);

const time = async (work) => {
  const start = performance.now();
  await work();
  return performance.now() - start;
};

// Warm the ratchets and the crypto implementations up.
for (let at = 0; at < 200; at += 1) await b.decrypt("1001", await a.encrypt(OPUS));

const encryptMs = await time(async () => {
  for (let at = 0; at < FRAMES; at += 1) await a.encrypt(OPUS);
});

// One sender's frames, opened once each: the receiver's side of the same stream.
const frames = [];
for (let at = 0; at < FRAMES; at += 1) frames.push(await a.encrypt(OPUS));
const decryptMs = await time(async () => {
  for (const frame of frames) await b.decrypt("1001", frame);
});

const encryptUs = (encryptMs * 1000) / FRAMES;
const decryptUs = (decryptMs * 1000) / FRAMES;
console.log(
  JSON.stringify(
    {
      runtime: Bun.version,
      jit: process.env.BUN_JSC_useJIT === "0" ? "off" : "default",
      frames: FRAMES,
      encrypt_us_per_frame: Number(encryptUs.toFixed(2)),
      decrypt_us_per_frame: Number(decryptUs.toFixed(2)),
      speakers_3_per_20ms_us: Number((encryptUs + 3 * decryptUs).toFixed(2)),
      budget_us: 20_000,
    },
    null,
    2,
  ),
);
