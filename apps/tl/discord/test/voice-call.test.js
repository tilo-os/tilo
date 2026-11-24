// A Discord voice channel carried into the workspace's call end to end on the Discord side: the
// bridge's call lane, its real voice connection to a fake Discord voice gateway and UDP server,
// through a fake of the sandbox's egress proxy. The lane stands in for the daemon's side, which
// the daemon's own tests drive through the SDK's client: it takes what the bridge reports and
// says, hands it the workspace's frames, and asks it in and out as the daemon does.

import { test, expect } from "bun:test";
import { startCalls } from "../src/call.js";
import { connect } from "../src/voice.js";
import { fakeProxy } from "./fake-socks.js";
import { USER, fakeVoiceServer } from "./fake-voice.js";

const HASH = "ab".repeat(32);

async function until(check, what, timeout = 4_000) {
  const end = Date.now() + timeout;
  while (!check()) {
    if (Date.now() > end) throw new Error(`waited for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

function account() {
  return {
    accounts: [
      {
        guild_id: "123",
        only_channel: "",
        map: { "channel:voice-tl": { kind: "channel", tilo: "voice-tl", discord: "458" } },
        channels: [{ id: "458", kind: "voice", name: "Call" }],
      },
    ],
  };
}

/// The workspace's side of the lane, as the daemon answers it: `refuse` is why it refuses the
/// carry, as a workspace that grants the app no call does.
function daemonLane({ refuse = null } = {}) {
  return {
    carried: [],
    reports: [],
    heardFrames: [],
    toSay: [],
    asks: [],
    carry(pair) {
      if (refuse && pair) return Promise.reject(new Error(refuse));
      this.carried.push(pair);
      return Promise.resolve();
    },
    report(report) {
      this.reports.push(structuredClone(report));
      return Promise.resolve();
    },
    heard(frame) {
      this.heardFrames.push(frame);
      return Promise.resolve();
    },
    said() {
      const said = this.toSay;
      this.toSay = [];
      return Promise.resolve(said);
    },
    asked() {
      return Promise.resolve(this.asks.shift() ?? null);
    },
    /// The people the bridge last said are in Discord's call.
    people() {
      return this.reports.at(-1)?.calls?.[0]?.people ?? [];
    },
  };
}

async function bridge({ refuse } = {}) {
  const voiceServer = await fakeVoiceServer();
  const proxy = await fakeProxy();
  const lane = daemonLane({ refuse });
  const recorded = [];
  const shown = [];
  const gateway = {
    user: { id: "900" },
    joined: [],
    left: [],
    joinVoice(guild, channel) {
      this.joined.push([guild, channel]);
      return Promise.resolve({
        guild,
        channel,
        endpoint: voiceServer.url,
        token: "voice-token",
        session: "voice-session",
      });
    },
    leaveVoice(guild) {
      this.left.push(guild);
    },
  };
  const calls = {
    state: () => Promise.resolve(account()),
    do(call) {
      recorded.push(call);
      return Promise.resolve({});
    },
  };
  const carried = startCalls({
    calls,
    lane,
    gateway,
    // The voice connection the bridge opens in its sandbox: its UDP through the egress proxy.
    makeVoice: (options) => connect({ ...options, allowLoopback: true, proxy: proxy.at }),
    // Discord's CDN, serving Dana's picture.
    fetch: () =>
      Promise.resolve(
        new Response(new Uint8Array([1, 2, 3]), {
          status: 200,
          headers: { "content-type": "image/png" },
        }),
      ),
    content: () => Promise.resolve(`tilo.content:${HASH}`),
    showStatus: (channel, status) => {
      shown.push([channel, status]);
      return Promise.resolve();
    },
    log: () => {},
  });
  return {
    voiceServer,
    proxy,
    lane,
    gateway,
    carried,
    recorded,
    shown,
    stop() {
      carried.close();
      proxy.stop();
      voiceServer.stop();
    },
  };
}

test("a Discord call and the workspace's call are one: people, voices, mute and leaving", async () => {
  const host = await bridge();
  try {
    await host.carried.start();
    expect(host.lane.carried).toEqual([{ outside: "458", tilo: "voice-tl" }]);

    // Dana is in Discord's voice channel: the workspace's call is told her name and picture.
    host.carried.hear({
      state: { guild: "123", user: USER, channel: "458", name: "Dana", avatar: "9f3" },
    });
    await until(() => host.lane.people()[0]?.avatar === HASH, "Dana with her picture");
    expect(host.lane.people()).toEqual([
      {
        account: USER,
        name: "Dana",
        avatar_url: `https://cdn.discordapp.com/avatars/${USER}/9f3.png?size=128`,
        avatar: HASH,
        muted: false,
        deafened: false,
      },
    ]);
    expect(host.lane.reports.at(-1).link).toEqual({ link: "idle" });

    // Someone is on either side: the daemon asks the bot in, and the bot joins Discord's voice
    // server, saying who is on the workspace's side as the channel's status.
    host.lane.asks.push({ call: "458", status: "On Tilo: Grace" });
    await until(() => host.voiceServer.seen.some((frame) => frame.op === 1), "the voice session");
    expect(host.gateway.joined).toEqual([["123", "458"]]);
    expect(host.shown).toEqual([["458", "On Tilo: Grace"]]);
    await until(
      () => host.lane.reports.at(-1)?.link?.link === "joined",
      "the bridge in Discord's call",
    );

    // What Dana says reaches the workspace's call as hers.
    host.voiceServer.say([9, 8, 7]);
    await until(() => host.lane.heardFrames.length >= 1, "Dana heard");
    expect(host.lane.heardFrames[0].speaker).toBe(USER);
    expect([...host.lane.heardFrames[0].opus]).toEqual([9, 8, 7]);

    // What the workspace's people say reaches Discord untouched, through the egress proxy.
    host.lane.toSay.push({ speaker: "aa".repeat(32), opus: Buffer.from([1, 2, 3, 4]) });
    await until(() => host.voiceServer.packets.length >= 1, "the workspace heard in Discord");
    expect([...host.voiceServer.packets[0].frame]).toEqual([1, 2, 3, 4]);
    expect(host.proxy.carried.length).toBeGreaterThan(0);

    // Dana mutes herself in Discord: the workspace's call shows her muted.
    host.carried.hear({
      state: { guild: "123", user: USER, channel: "458", name: "Dana", avatar: "9f3", self_mute: true },
    });
    await until(() => host.lane.people()[0]?.muted === true, "Dana muted");

    // Everyone left the workspace's side and Dana left Discord's: the daemon asks the bot out,
    // and it leaves Discord's call and clears the status. The pair stays carried.
    host.carried.hear({ state: { guild: "123", user: USER, channel: "" } });
    await until(() => host.lane.people().length === 0, "Dana gone from the call");
    host.lane.asks.push({ call: null, status: null });
    await until(() => host.gateway.left.length === 1, "the bot out of Discord's call");
    await until(() => host.lane.reports.at(-1)?.link?.link === "idle", "the bridge idle");
    expect(host.shown.at(-1)).toEqual(["458", ""]);
    host.carried.hear({ state: { guild: "123", user: "900", channel: "" } });
    expect(host.lane.carried).toEqual([{ outside: "458", tilo: "voice-tl" }]);

    // Someone joins again, and then Discord moves the bot out of the call: both sides end.
    host.lane.asks.push({ call: "458", status: "On Tilo: Grace" });
    await until(() => host.gateway.joined.length === 2, "the bot back in");
    await until(() => host.lane.reports.at(-1)?.link?.link === "joined", "the bridge joined again");
    host.carried.hear({ state: { guild: "123", user: "900", channel: "" } });
    await until(() => host.lane.carried.at(-1) === null, "the workspace's call let go");
    expect(host.recorded.some((call) => call.action === "report_voice")).toBe(true);
  } finally {
    host.stop();
  }
});

test("a workspace that grants the bridge no call is said so, and the bot joins nothing", async () => {
  const host = await bridge({ refuse: "this workspace grants the bridge no call" });
  try {
    await host.carried.start();
    await until(
      () => host.recorded.some((call) => call.action === "report_voice"),
      "the refusal recorded",
    );
    const problem = host.recorded.find((call) => call.action === "report_voice").args.problem;
    expect(problem).toBe("this workspace grants the bridge no call");
    expect(host.lane.reports.at(-1).link).toEqual({
      link: "refused",
      call: "458",
      reason: "this workspace grants the bridge no call",
    });
    host.lane.asks.push({ call: "458", status: "On Tilo: Grace" });
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(host.gateway.joined).toEqual([]);
    expect(host.voiceServer.seen).toEqual([]);
  } finally {
    host.stop();
  }
});
