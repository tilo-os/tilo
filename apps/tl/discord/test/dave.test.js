import { test, expect } from "bun:test";
import { SILENCE, connect } from "../src/voice.js";
import {
  BRIDGE,
  CHANNEL,
  OTHER,
  USER,
  daveVoiceServer,
  member,
} from "./fake-dave-voice.js";

/// Wait until `check()` is true, or fail after `timeout`.
async function until(check, what, timeout = 4_000) {
  const end = Date.now() + timeout;
  while (!check()) {
    if (Date.now() > end) throw new Error(`waited for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

/// A voice connection to `fake`, recording the frames it hears.
function join(fake) {
  const heard = [];
  let ended = null;
  const voice = connect({
    endpoint: fake.url,
    guild: "123",
    channel: CHANNEL,
    user: BRIDGE,
    session: "session-1",
    token: "token-1",
    allowLoopback: true,
    onAudio: (frame) => heard.push(frame),
    onEnd: (reason) => {
      ended = reason;
    },
    log: (line) => console.error(`test: ${line}`),
  });
  fake.attach(voice);
  return { voice, heard, ended: () => ended };
}

test("a DAVE call: the group is founded, a member is welcomed, and audio crosses sealed", async () => {
  const fake = await daveVoiceServer();
  const call = join(fake);
  try {
    // The gateway's external sender and the bridge's key package.
    await until(() => fake.frames.some((frame) => frame.op === 26), "the bridge's key package");
    expect(call.voice.dave.version).toBe(1);

    // The second person in the call joins the group: the gateway proposes them, the bridge
    // commits, and the welcome lands with them.
    const other = await member({ user: USER });
    await other.setExternalSender(fake.gate.payload);
    await fake.propose(other);
    await until(() => fake.frames.some((frame) => frame.op === 28), "the commit and welcome");
    const first = fake.takeCommit();
    expect(first.welcome).not.toBeNull();
    await other.processWelcome(1, first.welcome);

    // The gateway announces the commit and executes the transition; both sides now hold it.
    await fake.announce(1, first.commit);
    await fake.execute(1);
    await until(() => call.voice.dave.sending() && other.sending(), "the first epoch's keys");
    expect(call.voice.dave.members()).toBe(2);
    expect(other.members()).toBe(2);

    // What the other person says reaches the workspace, decrypted, as them.
    const theirs = [9, 8, 7, 6, 5];
    await fake.say(other, theirs);
    await until(() => call.heard.length >= 1, "the outside frame");
    expect(call.heard[0].speaker).toBe(USER);
    expect([...call.heard[0].opus]).toEqual(theirs);

    // What the workspace says reaches them, sealed under the bridge's own ratchet.
    call.voice.say(new Uint8Array([1, 2, 3, 4]));
    await until(() => fake.packets.length >= 1, "a packet on the wire");
    const opened = await other.decrypt(BRIDGE, fake.packets[0].frame);
    expect(opened && [...opened]).toEqual([1, 2, 3, 4]);

    // A tampered frame opens nothing.
    const tampered = Buffer.from(fake.packets[0].frame);
    tampered[0] ^= 0xff;
    expect(await other.decrypt(BRIDGE, tampered)).toBeNull();
  } finally {
    call.voice.close();
    fake.stop();
  }
});

test("a DAVE call moves to a new epoch and the new keys carry the audio", async () => {
  const fake = await daveVoiceServer();
  const call = join(fake);
  try {
    await until(() => fake.frames.some((frame) => frame.op === 26), "the bridge's key package");
    const other = await member({ user: USER });
    await other.setExternalSender(fake.gate.payload);
    await fake.propose(other);
    await until(() => fake.frames.some((frame) => frame.op === 28), "the commit and welcome");
    const first = fake.takeCommit();
    await other.processWelcome(1, first.welcome);
    await fake.announce(1, first.commit);
    await fake.execute(1);
    await until(() => call.voice.dave.sending() && other.sending(), "the first epoch's keys");

    // A third person joins: a new commit moves the group to its next epoch.
    const third = await member({ user: OTHER });
    await third.setExternalSender(fake.gate.payload);
    await fake.propose(third);
    await until(
      () => fake.frames.filter((frame) => frame.op === 28).length >= 2,
      "the second commit",
    );
    const second = fake.takeCommit();
    await third.processWelcome(2, second.welcome);
    await fake.announce(2, second.commit);
    await fake.execute(2);
    await until(
      () =>
        call.voice.dave.members() === 3 && other.members() === 3 && third.members() === 3,
      "the second epoch's keys",
    );

    // The newest member's audio opens under the new epoch, and the oldest still speaks to the
    // new one.
    const fresh = [4, 3, 2, 1];
    await fake.say(third, fresh);
    await until(() => call.heard.length >= 1, "an outside frame in the new epoch");
    expect([...call.heard[0].opus]).toEqual(fresh);

    call.voice.say(new Uint8Array([7, 7]));
    await until(() => fake.packets.length >= 1, "a packet in the new epoch");
    expect([...(await third.decrypt(BRIDGE, fake.packets[0].frame))]).toEqual([7, 7]);
  } finally {
    call.voice.close();
    fake.stop();
  }
});

test("before the keys are in place a DAVE call sends nothing, and silence passes as it is", async () => {
  const fake = await daveVoiceServer();
  const call = join(fake);
  try {
    await until(() => call.voice.dave.version === 1, "the DAVE protocol version");
    call.voice.say(new Uint8Array([1]));
    await new Promise((resolve) => setTimeout(resolve, 60));
    expect(fake.packets).toHaveLength(0);

    // Silence is not encrypted even once the call speaks the protocol.
    const other = await member({ user: USER });
    await other.setExternalSender(fake.gate.payload);
    await fake.propose(other);
    await until(() => fake.frames.some((frame) => frame.op === 28), "the commit and welcome");
    const first = fake.takeCommit();
    await other.processWelcome(1, first.welcome);
    await fake.announce(1, first.commit);
    await fake.execute(1);
    await until(() => call.voice.dave.sending(), "the first epoch's keys");
    await fake.say(other, SILENCE);
    call.voice.say(new Uint8Array([2, 2]));
    await new Promise((resolve) => setTimeout(resolve, 80));
    expect(call.heard).toHaveLength(0);
  } finally {
    call.voice.close();
    fake.stop();
  }
});
