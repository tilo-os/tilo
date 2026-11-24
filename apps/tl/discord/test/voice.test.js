import { test, expect } from "bun:test";
import { createSocket } from "node:dgram";
import { AES_GCM, Cipher, Sender, open } from "../src/rtp.js";
import { SILENCE, closeMeaning, connect, voiceUrl } from "../src/voice.js";

const KEY = new Uint8Array(32).fill(3);
const SSRC = 1234;
const USER = "7001";

/// Wait until `check()` is true, or fail after `timeout`.
async function until(check, what, timeout = 4_000) {
  const end = Date.now() + timeout;
  while (!check()) {
    if (Date.now() > end) throw new Error(`waited for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

/// A fake Discord voice server: a WebSocket gateway handing out a UDP address, and that UDP
/// socket doing IP discovery and carrying sealed RTP both ways.
function fakeVoiceServer({ modes = [AES_GCM], ssrc = SSRC } = {}) {
  const seen = [];
  const packets = [];
  let client = null;
  let ws = null;
  let udp = null;
  let server = null;
  const cipher = new Cipher(AES_GCM, KEY);
  const speaking = [];

  const socket = createSocket("udp4");
  socket.on("message", (packet, from) => {
    // Discovery: op 1 (request), 70 bytes of padding, the SSRC.
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

  return new Promise((resolve) => {
    socket.bind(0, "127.0.0.1", () => {
      const port = socket.address().port;
      server = Bun.serve({
        port: 0,
        fetch(request, server) {
          if (server.upgrade(request)) return;
          return new Response("a voice gateway speaks WebSocket alone", { status: 400 });
        },
        websocket: {
          open(socket) {
            ws = socket;
            socket.send(JSON.stringify({ op: 8, d: { heartbeat_interval: 60 } }));
          },
          message(socket, raw) {
            const frame = JSON.parse(String(raw));
            seen.push(frame);
            if (frame.op === 3) {
              socket.send(JSON.stringify({ op: 6, d: {} }));
              return;
            }
            if (frame.op === 0) {
              socket.send(
                JSON.stringify({
                  op: 2,
                  d: { ssrc, ip: "127.0.0.1", port, modes, heartbeat_interval: 60 },
                }),
              );
              return;
            }
            if (frame.op === 1) {
              socket.send(
                JSON.stringify({
                  op: 4,
                  d: { mode: frame.d.data.mode, secret_key: [...KEY], dave_protocol_version: 0 },
                }),
              );
              // Who speaks on the SSRC this fake sends audio under, as Discord's own gateway
              // says it before any frame arrives.
              socket.send(JSON.stringify({ op: 5, d: { ssrc: 1, user_id: USER } }));
              return;
            }
            if (frame.op === 5) speaking.push(frame.d);
          },
        },
      });
      resolve({
        url: `127.0.0.1:${server.port}`,
        seen,
        packets,
        speaking,
        /// One sealed packet from `USER`, as the voice server sends audio.
        say(opus, { user = USER } = {}) {
          const sender = new Sender(1, [0, 1, 0, 0, 0, 2, 1, 2, 3, 4]);
          const packet = sender.packet(cipher, Buffer.from(opus));
          if (!client) throw new Error("no client address yet");
          socket.send(packet, client.port, client.address);
        },
        stop() {
          socket.close();
          server.stop(true);
        },
      });
    });
  });
}

/// A voice connection to `fake`, recording the frames it hears and where it ended.
function join(fake, options = {}) {
  const heard = [];
  let ended = null;
  const voice = connect({
    endpoint: fake.url,
    guild: "123",
    channel: "458",
    user: "900",
    session: "session-1",
    token: "token-1",
    allowLoopback: true,
    onAudio: (frame) => heard.push(frame),
    onEnd: (reason) => {
      ended = reason;
    },
    log: () => {},
    ...options,
  });
  return {
    voice,
    heard,
    ended: () => ended,
  };
}

test("a call identifies, discovers its address, and seals both directions", async () => {
  const fake = await fakeVoiceServer();
  const call = join(fake);
  try {
    await until(() => fake.seen.some((frame) => frame.op === 1), "select protocol");
    const identify = fake.seen.find((frame) => frame.op === 0);
    expect(identify.d.max_dave_protocol_version).toBe(1);
    expect(identify.d.channel_id).toBeUndefined();
    expect(identify.d.server_id).toBe("123");
    expect(identify.d.user_id).toBe("900");

    // The workspace's voice reaches Discord as it is: the same Opus bytes, one stream.
    call.voice.say(new Uint8Array([1, 2, 3, 4]));
    await until(() => fake.packets.length >= 1, "a packet on the wire");
    expect([...fake.packets[0].frame]).toEqual([1, 2, 3, 4]);
    await until(() => fake.speaking.some((said) => said.speaking === 1), "the speaking op");

    // What Discord says reaches the workspace as the person who said it.
    fake.say([9, 8, 7]);
    await until(() => call.heard.length >= 1, "an outside frame");
    expect(call.heard[0].speaker).toBe(USER);
    expect([...call.heard[0].opus]).toEqual([9, 8, 7]);

    // A pause is silence for a moment, then the microphone is off.
    call.voice.say(new Uint8Array([5]));
    await until(() => fake.packets.length >= 2, "the second packet");
    await until(
      () => fake.packets.some((packet) => packet.frame.equals(SILENCE)),
      "silence after the last frame",
    );
    await until(
      () => fake.speaking.some((said) => said.speaking === 0),
      "the microphone turned off",
    );
    expect(call.ended()).toBeNull();
  } finally {
    call.voice.close();
    fake.stop();
  }
});

test("a voice server that offers no mode this build has ends the call with the reason", async () => {
  const fake = await fakeVoiceServer({ modes: ["aead_xchacha20_poly1305_rtpsize"] });
  const call = join(fake);
  try {
    await until(() => call.ended() !== null, "the call to end");
    expect(call.ended()).toContain("aead_xchacha20_poly1305_rtpsize");
  } finally {
    call.voice.close();
    fake.stop();
  }
});

test("a muted call sends nothing and hears nothing", async () => {
  const fake = await fakeVoiceServer();
  const call = join(fake);
  try {
    await until(() => fake.seen.some((frame) => frame.op === 1), "select protocol");
    call.voice.setMuted(true);
    call.voice.say(new Uint8Array([4, 4]));
    fake.say([6, 6]);
    await new Promise((resolve) => setTimeout(resolve, 120));
    expect(fake.packets).toHaveLength(0);
    expect(call.heard).toHaveLength(0);
  } finally {
    call.voice.close();
    fake.stop();
  }
});

test("a voice endpoint that is not Discord's is refused before anything connects", () => {
  expect(voiceUrl("c-sjc01.discord.media:2096")).toBe("wss://c-sjc01.discord.media/?v=8");
  expect(voiceUrl("c-sjc01.discord.media")).toBe("wss://c-sjc01.discord.media/?v=8");
  expect(() => voiceUrl("evil.example:2096")).toThrow("not one Discord names");
  expect(() => voiceUrl("127.0.0.1:34187")).toThrow("not one Discord names");
  expect(voiceUrl("127.0.0.1:34187", { allowLoopback: true })).toBe(
    "ws://127.0.0.1:34187/?v=8",
  );
  expect(() => voiceUrl("")).toThrow("no voice server");
});

test("close codes resume, identify, or end the call for good", () => {
  expect(closeMeaning(4015, "")).toEqual({ again: "resume" });
  expect(closeMeaning(1006, "")).toEqual({ again: "resume" });
  expect(closeMeaning(4006, "")).toEqual({ again: "identify" });
  expect(closeMeaning(4009, "")).toEqual({ again: "identify" });
  expect(closeMeaning(4014, "").end).toContain("disconnected");
  expect(closeMeaning(4017, "E2EE/DAVE protocol required").end).toContain("DAVE");
  expect(closeMeaning(4021, "").end).toContain("rate-limiting");
  expect(closeMeaning(4000, "weird").end).toBeUndefined();
});
