// A fake Discord voice server that speaks DAVE: it hands out its own external sender, proposes
// the people it serves to the group, announces the commits it broadcasts, and executes the
// transitions. The people are real DaveSessions, so the frames it carries are end-to-end
// encrypted between two members exactly as Discord's would be.

import { createSocket } from "node:dgram";
import { AES_GCM, Cipher, Sender, open } from "../src/rtp.js";
import { DaveSession, SUITE, daveCryptoProvider, p256, userIdBytes } from "../src/dave.js";
import { protectExternalProposalPublic } from "../vendor/ts-mls/messageProtectionPublic.js";
import { encodeMlsMessage, decodeMlsMessage } from "../vendor/ts-mls/message.js";
import { encodeWelcome, decodeWelcome } from "../vendor/ts-mls/welcome.js";
import { getCiphersuiteImpl } from "../vendor/ts-mls/crypto/getCiphersuiteImpl.js";
import { getCiphersuiteFromName } from "../vendor/ts-mls/crypto/ciphersuite.js";
import { lengthEncoder } from "../vendor/ts-mls/codec/variableLength.js";

const KEY = new Uint8Array(32).fill(3);
const SSRC = 1234;
export const USER = "7001";
export const OTHER = "7002";
export const BRIDGE = "900";
export const CHANNEL = "458";

/// An MLS vector: its length header, then `bytes`.
export function vector(bytes) {
  const [length, write] = lengthEncoder(bytes.length);
  const out = new Uint8Array(length + bytes.length);
  write(0, out.buffer);
  out.set(bytes, length);
  return Buffer.from(out);
}

/// The MLS implementation the fake gateway signs external proposals with.
export async function testImplementation() {
  return getCiphersuiteImpl(getCiphersuiteFromName(SUITE), daveCryptoProvider);
}

/// The gateway's own credential as the external sender of a call.
export async function gatewayIdentity() {
  const keys = await p256.keygen();
  const credential = { credentialType: "basic", identity: userIdBytes("1") };
  const payload = Buffer.concat([
    vector(keys.publicKey),
    Buffer.from([0, 1]),
    vector(credential.identity),
  ]);
  return { keys, credential, payload };
}

/// A member of the call, as Discord's clients are: the bridge is one, and every other person
/// in the call is another.
export async function member({ user, channel = CHANNEL, send = () => {} }) {
  const session = new DaveSession({
    userId: user,
    channelId: channel,
    send,
    ready: () => {},
    log: () => {},
  });
  await session.setVersion(1);
  return session;
}

/// What a gateway sends as a proposal to add `keyPackage` to the group whose context is `gc`.
export async function addProposal(gate, gc, keyPackage, impl) {
  const protectedMessage = await protectExternalProposalPublic(
    gate.keys.signKey,
    gc,
    new Uint8Array(),
    { proposalType: "add", add: { keyPackage } },
    { senderType: "external", senderIndex: 0 },
    impl,
  );
  const message = Buffer.from(
    encodeMlsMessage({
      version: "mls10",
      wireformat: "mls_public_message",
      publicMessage: protectedMessage.publicMessage,
    }),
  );
  return Buffer.concat([Buffer.from([0]), vector(message), message]);
}

/// A fake voice server with a DAVE gateway and a UDP socket carrying sealed RTP both ways.
export async function daveVoiceServer({ user = USER, ssrc = SSRC } = {}) {
  const gate = await gatewayIdentity();
  const impl = await testImplementation();
  const seen = [];
  const frames = [];
  const packets = [];
  const speaking = [];
  const sent = [];
  let client = null;
  let ws = null;
  let server = null;
  let seq = 0;
  const cipher = new Cipher(AES_GCM, KEY);
  const sender = new Sender(1, [0, 1, 0, 0, 0, 2, 1, 2, 3, 4]);
  /// The bridge's own session, read for its group context: the gateway tracks the group id,
  /// epoch and ciphersuite, and the test takes the rest from the member it drives.
  let bridge = null;
  const people = [];
  /// Each member speaks on its own SSRC, as a voice gateway reports them.
  const senders = new Map();

  const socket = createSocket("udp4");
  socket.on("message", (packet, from) => {
    if (packet.length === 74 && packet.readUInt16BE(0) === 1) {
      client = from;
      const answer = Buffer.alloc(74);
      answer.writeUInt16BE(2, 0);
      answer.writeUInt16BE(70, 2);
      answer.writeUInt32BE(packet.readUInt32BE(4), 4);
      answer.write(from.address, 8, "utf8");
      answer.writeUInt16BE(from.port, 72);
      socket.send(answer, from.port, from.address);
      return;
    }
    const opened = open(cipher, packet);
    if (opened) packets.push(opened);
  });

  const send = (op, payload = Buffer.alloc(0)) => {
    seq = (seq + 1) & 0xffff;
    const header = Buffer.alloc(3);
    header.writeUInt16BE(seq, 0);
    header[2] = op;
    ws?.send(Buffer.concat([header, Buffer.from(payload)]));
  };
  const sendJson = (op, d = {}) => ws?.send(JSON.stringify({ op, d }));

  return new Promise((resolve) => {
    socket.bind(0, "127.0.0.1", () => {
      const port = socket.address().port;
      server = Bun.serve({
        port: 0,
        fetch(request, upgraded) {
          if (upgraded.upgrade(request)) return;
          return new Response("a voice gateway speaks WebSocket alone", { status: 400 });
        },
        websocket: {
          open(socket) {
            ws = socket;
            sendJson(8, { heartbeat_interval: 60 });
          },
          message(socket, raw) {
            if (typeof raw === "string") {
              const frame = JSON.parse(raw);
              seen.push(frame);
              if (frame.op === 3) return sendJson(6, {});
              if (frame.op === 0) {
                sendJson(2, { ssrc, ip: "127.0.0.1", port, modes: [AES_GCM], heartbeat_interval: 60 });
                return;
              }
              if (frame.op === 1) {
                sendJson(4, {
                  mode: frame.d.data.mode,
                  secret_key: [...KEY],
                  dave_protocol_version: 1,
                });
                sendJson(5, { ssrc: 1, user_id: user });
                send(25, gate.payload);
                return;
              }
              if (frame.op === 5) speaking.push(frame.d);
              if (frame.op === 23) sent.push({ ready: frame.d.transition_id });
              return;
            }
            const bytes = Buffer.from(raw);
            const op = bytes[0];
            const payload = bytes.subarray(1);
            frames.push({ op, payload });
          },
        },
      });
      resolve({
        url: `127.0.0.1:${server.port}`,
        gate,
        impl,
        seen,
        frames,
        packets,
        speaking,
        sent,
        users: people,
        /// The bridge's session, which the gateway reads its group context from.
        attach(session) {
          bridge = session;
        },
        /// Add `person` to the call: the gateway proposes them to the group, to every member
        /// of it, as a voice gateway does.
        async propose(person) {
          people.push(person);
          sendJson(5, { ssrc: people.length, user_id: person.userId });
          senders.set(person, new Sender(people.length, [0, 1, 0, 0, 0, 2, 1, 2, 3, 4]));
          if (!bridge?.dave.state) return;
          const proposals = await addProposal(
            gate,
            bridge.dave.state.groupContext,
            person.keyPackage.publicPackage,
            impl,
          );
          send(27, proposals);
          for (const member of people) {
            if (member !== person) await member.processProposals(proposals);
          }
        },
        /// The commit and welcome the bridge answered a proposal with (op 28).
        takeCommit() {
          const frame = frames.filter((entry) => entry.op === 28).pop();
          if (!frame) return null;
          const decoded = decodeMlsMessage(frame.payload, 0);
          if (!decoded) return null;
          const [message, read] = decoded;
          const welcome = decodeWelcome(frame.payload.subarray(read), 0)?.[0] ?? null;
          return {
            commit: Buffer.from(encodeMlsMessage(message)),
            welcome: welcome ? Buffer.from(encodeWelcome(welcome)) : null,
          };
        },
        /// Announce a commit to every member that is in the group it moves, as the voice
        /// gateway does (op 29). The members added by it are welcomed instead.
        async announce(transitionId, commit) {
          const header = Buffer.alloc(2);
          header.writeUInt16BE(transitionId, 0);
          send(29, Buffer.concat([header, commit]));
          const content = decodeMlsMessage(commit, 0)?.[0]?.publicMessage?.content;
          const from = content?.epoch;
          for (const person of people) {
            if (!person.state || from === undefined) continue;
            if (person.state.groupContext.epoch !== from) continue;
            await person.processCommit(transitionId, commit);
          }
        },
        /// Execute a prepared transition everywhere (op 22).
        async execute(transitionId) {
          sendJson(22, { transition_id: transitionId });
          for (const person of people) await person.executeTransition(transitionId);
        },
        /// One Opus frame said by `person`, sealed end to end and carried as RTP.
        async say(person, opus) {
          const frame = await person.encrypt(Buffer.from(opus));
          if (!client) throw new Error("no client address yet");
          const speaking = senders.get(person) ?? sender;
          socket.send(speaking.packet(cipher, frame), client.port, client.address);
        },
        stop() {
          socket.close();
          server.stop(true);
        },
      });
    });
  });
}
