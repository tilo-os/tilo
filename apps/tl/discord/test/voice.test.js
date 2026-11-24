import { test, expect } from "bun:test";
import { fakeProxy } from "./fake-socks.js";
import { USER, fakeVoiceServer } from "./fake-voice.js";
import { SILENCE, closeMeaning, connect, voiceUrl } from "../src/voice.js";

/// Wait until `check()` is true, or fail after `timeout`.
async function until(check, what, timeout = 4_000) {
  const end = Date.now() + timeout;
  while (!check()) {
    if (Date.now() > end) throw new Error(`waited for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
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
    // These tests speak to a voice server directly; the sandbox's proxy has its own test.
    proxy: null,
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

test("a call's media crosses the egress proxy's UDP association", async () => {
  const fake = await fakeVoiceServer();
  const proxy = await fakeProxy();
  const call = join(fake, { proxy: proxy.at });
  try {
    await until(() => fake.seen.some((frame) => frame.op === 1), "select protocol");
    call.voice.say(new Uint8Array([1, 2, 3, 4]));
    await until(() => fake.packets.length >= 1, "a packet on the wire");
    expect([...fake.packets[0].frame]).toEqual([1, 2, 3, 4]);
    // The proxy was asked for a UDP association, and the media went through it.
    expect(proxy.requests.map((request) => request.command ?? request.greeting[0])).toEqual([
      5, 3,
    ]);
    expect(proxy.carried.length).toBeGreaterThan(0);
    // What Discord says comes back the same way.
    fake.say([9, 8, 7]);
    await until(() => call.heard.length >= 1, "an outside frame");
    expect([...call.heard[0].opus]).toEqual([9, 8, 7]);
    expect(call.ended()).toBeNull();
  } finally {
    call.voice.close();
    proxy.stop();
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
