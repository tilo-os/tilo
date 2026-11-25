import { test, expect } from "bun:test";
import { startCalls } from "../src/call.js";

/// Wait until `check()` is true, or fail after `timeout`.
async function until(check, what, timeout = 4_000) {
  const end = Date.now() + timeout;
  while (!check()) {
    if (Date.now() > end) throw new Error(`waited for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

/// The bridge's record: one Discord account whose voice channel `458` is the workspace channel
/// `voice-tl`.
function account() {
  return {
    accounts: [
      {
        guild_id: "123",
        only_channel: "",
        map: {
          "channel:voice-tl": { kind: "channel", tilo: "voice-tl", discord: "458" },
          "channel:text": { kind: "channel", tilo: "text", discord: "456" },
        },
        channels: [
          { id: "456", kind: "text", name: "design" },
          { id: "458", kind: "voice", name: "Call" },
        ],
      },
    ],
  };
}

/// What one carried call did, as the workspace's lane and Discord's gateway saw it.
function harness({ makeVoice } = {}) {
  const lane = {
    carried: [],
    reports: [],
    heardFrames: [],
    saidFrames: [],
    asks: [],
    carry(pair) {
      this.carried.push(pair);
      return Promise.resolve({});
    },
    report(report) {
      this.reports.push(report);
      return Promise.resolve({});
    },
    heard(frame) {
      this.heardFrames.push(frame);
      return Promise.resolve();
    },
    said() {
      const said = this.saidFrames;
      this.saidFrames = [];
      return Promise.resolve(said);
    },
    asked() {
      return Promise.resolve(this.asks.shift() ?? null);
    },
  };
  const calls = {
    state: () => Promise.resolve(account()),
    done: [],
    do(call) {
      this.done.push(call);
      return Promise.resolve({});
    },
  };
  const gateway = {
    user: { id: "900" },
    joined: [],
    left: [],
    joinVoice(guild, channel) {
      this.joined.push([guild, channel]);
      return Promise.resolve({
        guild,
        channel,
        endpoint: "c-sjc01.discord.media",
        token: "token-1",
        session: "session-1",
      });
    },
    leaveVoice(guild) {
      this.left.push(guild);
    },
  };
  const voices = [];
  const opened = async (options) => {
    const voice = {
      options,
      said: [],
      closed: false,
      say(opus) {
        this.said.push(Buffer.from(opus));
      },
      close() {
        this.closed = true;
      },
    };
    voices.push(voice);
    return voice;
  };
  const shown = [];
  const carried = startCalls({
    calls,
    lane,
    gateway,
    makeVoice: makeVoice ?? opened,
    // Discord's CDN is out of reach: a person's avatar stays where Discord serves it.
    fetch: async () => ({ status: 404, ok: false }),
    // Discord's REST, as the vault makes the call: the lines shown on the channel's status.
    webCall: (request) => {
      const channel = /channels\/([^/]+)\/voice-status$/.exec(request.url)?.[1];
      if (channel) shown.push([channel, JSON.parse(request.body).status ?? ""]);
      return Promise.resolve({ status: 204 });
    },
    log: () => {},
  });
  return { lane, calls, gateway, voices, carried, shown };
}

test("a paired voice channel is carried: the outside call joins the workspace's call", async () => {
  const host = harness();
  try {
    await host.carried.start();
    // The pair is carried at once; the bot waits in no empty Discord call.
    expect(host.lane.carried).toEqual([{ outside: "458", tilo: "voice-tl" }]);
    await until(() => host.lane.reports.length >= 1, "the call's first report");
    expect(host.lane.reports[0].link).toEqual({ link: "idle" });
    expect(host.gateway.joined).toEqual([]);

    // Someone is in the workspace's call: the daemon asks the bot in, saying who is there.
    host.lane.asks.push({ call: "458", status: "On Tilo: Grace" });
    await until(() => host.voices.length === 1, "the voice connection");
    expect(host.gateway.joined).toEqual([["123", "458"]]);
    expect(host.shown).toEqual([["458", "On Tilo: Grace"]]);
    expect(host.voices[0].options).toMatchObject({
      endpoint: "c-sjc01.discord.media",
      channel: "458",
      user: "900",
    });

    // Who is in Discord's call becomes the workspace's bridged participants, with Discord's
    // name and mute for each.
    host.carried.hear({
      state: {
        guild: "123",
        user: "7001",
        channel: "458",
        name: "Dana",
        avatar: "abc",
        self_mute: true,
      },
    });
    await until(
      () => host.lane.reports.at(-1)?.calls?.[0]?.people?.length === 1,
      "the call's people",
    );
    const report = host.lane.reports.at(-1);
    expect(report.calls).toEqual([
      {
        call: "458",
        people: [
          {
            account: "7001",
            name: "Dana",
            avatar_url: "https://cdn.discordapp.com/avatars/7001/abc.png?size=128",
            muted: true,
            deafened: false,
          },
        ],
      },
    ]);
    expect(report.link).toMatchObject({ link: "joined", call: "458" });

    // What Discord says reaches the workspace as the person who said it.
    host.voices[0].options.onAudio({ speaker: "7001", opus: Buffer.from([1, 2]) });
    await until(() => host.lane.heardFrames.length === 1, "the outside frame");
    expect(host.lane.heardFrames[0]).toEqual({ speaker: "7001", opus: Buffer.from([1, 2]) });

    // What the workspace says reaches Discord untouched.
    host.lane.saidFrames = [{ speaker: "aa", opus: Buffer.from([3, 4]) }];
    await until(() => host.voices[0].said.length === 1, "the workspace's frame");
    expect([...host.voices[0].said[0]]).toEqual([3, 4]);

    // Nobody is left on either side: the bot leaves Discord's call and clears its status, and the
    // pair stays carried for whoever comes next.
    host.lane.asks.push({ call: null, status: null });
    await until(() => host.voices[0].closed, "the voice connection closed");
    expect(host.gateway.left).toEqual(["123"]);
    expect(host.shown.at(-1)).toEqual(["458", ""]);
    await until(() => host.lane.reports.at(-1)?.link?.link === "idle", "the bot out of the call");
    // Discord says the bot left, as it does for any leave: the bridge left it itself.
    host.carried.hear({ state: { guild: "123", user: "900", channel: "" } });
    expect(host.lane.carried).toEqual([{ outside: "458", tilo: "voice-tl" }]);

    // Someone comes back: the bot joins again.
    host.lane.asks.push({ call: "458", status: "On Tilo: Grace" });
    await until(() => host.voices.length === 2, "the voice connection again");
  } finally {
    host.carried.close();
  }
});

test("a call Discord refuses is reported in the bridge's record", async () => {
  const host = harness({
    makeVoice: () => Promise.reject(new Error("Discord requires end-to-end encryption (DAVE)")),
  });
  try {
    await host.carried.start();
    host.lane.asks.push({ call: "458", status: "On Tilo: Grace" });
    await until(
      () => host.calls.done.some((call) => call.action === "report_voice"),
      "the problem recorded",
    );
    const recorded = host.calls.done.find((call) => call.action === "report_voice");
    expect(recorded.args.problem).toContain("DAVE");
    const last = host.lane.reports.at(-1);
    expect(last.link).toMatchObject({ link: "refused", call: "458" });
    expect(last.link.reason).toContain("DAVE");
  } finally {
    host.carried.close();
  }
});

test("the bridge leaving the call in Discord ends both sides", async () => {
  const host = harness();
  try {
    await host.carried.start();
    host.lane.asks.push({ call: "458", status: "On Tilo: Grace" });
    await until(() => host.voices.length === 1, "the voice connection");
    host.carried.hear({ state: { guild: "123", user: "900", channel: "" } });
    await until(() => host.voices[0].closed, "the voice connection closed");
    await until(() => host.lane.carried.at(-1) === null, "the call let go");
    expect(host.calls.done.some((call) => call.action === "report_voice")).toBe(true);
  } finally {
    host.carried.close();
  }
});

test("a server with no paired voice channel opens no call", async () => {
  const host = harness();
  host.calls.state = () =>
    Promise.resolve({
      accounts: [{ guild_id: "123", map: {}, channels: [] }],
    });
  try {
    await host.carried.start();
    expect(host.voices).toHaveLength(0);
    expect(host.lane.carried).toEqual([]);
  } finally {
    host.carried.close();
  }
});
