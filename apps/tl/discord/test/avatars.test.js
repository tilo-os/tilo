import { test, expect } from "bun:test";
import { AVATAR_MAX_BYTES, avatarAddress, contentHash, keepAvatar } from "../src/avatars.js";
import { callPeople, voicePairs } from "../src/plan.js";
import { startCalls } from "../src/call.js";

const HASH = "ab".repeat(32);
const AVATAR = "https://cdn.discordapp.com/avatars/7001/9f3.png?size=128";
const BYTES = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 1, 2, 3]);

/// Wait until `check()` is true, or fail after `timeout`.
async function until(check, what, timeout = 4_000) {
  const end = Date.now() + timeout;
  while (!check()) {
    if (Date.now() > end) throw new Error(`waited for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

/// A fetch that answers `body` for every address, recording what it was asked for.
function pictureFetch({ status = 200, type = "image/png", body = BYTES, length } = {}) {
  const asked = [];
  const fetch = async (url) => {
    asked.push(String(url));
    const headers = new Headers({ "content-type": type });
    if (length !== undefined) headers.set("content-length", String(length));
    return new Response(body, { status, headers });
  };
  return { fetch, asked };
}

test("an avatar is fetched from Discord's CDN into the workspace's content", async () => {
  const { fetch, asked } = pictureFetch();
  const kept = [];
  const content = async (bytes) => {
    kept.push(bytes);
    return `tilo.content:${HASH}`;
  };
  const hash = await keepAvatar(AVATAR, { fetch, content });
  expect(hash).toBe(HASH);
  expect(asked).toEqual([AVATAR]);
  expect([...kept[0]]).toEqual([...BYTES]);
});

test("an avatar no one asked Discord for is not fetched, and a bad answer is not kept", async () => {
  const { fetch, asked } = pictureFetch();
  const content = async () => `tilo.content:${HASH}`;
  expect(await keepAvatar("https://evil.example/a.png", { fetch, content })).toBeNull();
  expect(asked).toHaveLength(0);
  expect(avatarAddress("http://cdn.discordapp.com/avatars/1/a.png")).toBeNull();
  expect(avatarAddress("https://cdn.discordapp.com.evil.example/a.png")).toBeNull();

  expect(await keepAvatar(AVATAR, { fetch: pictureFetch({ status: 404 }).fetch, content })).toBeNull();
  expect(
    await keepAvatar(AVATAR, { fetch: pictureFetch({ type: "text/html" }).fetch, content }),
  ).toBeNull();
  expect(
    await keepAvatar(AVATAR, {
      fetch: pictureFetch({ type: "image/png", length: AVATAR_MAX_BYTES + 1 }).fetch,
      content,
    }),
  ).toBeNull();
  expect(
    await keepAvatar(AVATAR, { fetch: pictureFetch({ body: new Uint8Array() }).fetch, content }),
  ).toBeNull();
  expect(await keepAvatar(AVATAR, { fetch, content: async () => "not a hash" })).toBeNull();
});

test("a content reference is trimmed to its bare hash", () => {
  expect(contentHash(`tilo.content:${HASH}`)).toBe(HASH);
  expect(contentHash(HASH)).toBe(HASH);
  expect(contentHash("tilo.content:xyz")).toBeNull();
  expect(contentHash(undefined)).toBeNull();
});

test("who is in a carried call says where their avatar is served and its hash here", () => {
  const avatars = new Map([[AVATAR, HASH]]);
  const people = callPeople(
    [
      { guild: "123", channel: "458", user: "7001", name: "Ada", avatar: "9f3" },
      { guild: "123", channel: "458", user: "7002", name: "Bo", avatar: "" },
    ],
    { guild: "123", channel: "458", self: "900", avatars },
  );
  expect(people[0]).toMatchObject({ account: "7001", avatar_url: AVATAR, avatar: HASH });
  expect(people[1]).toMatchObject({ account: "7002", avatar_url: "" });
  expect(people[1].avatar).toBeUndefined();
});

/// The bridge's record: one Discord account whose voice channel `458` is the workspace's call.
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

test("presence carries the avatar's hash once the picture is in the workspace", async () => {
  const reports = [];
  const lane = {
    carry: () => Promise.resolve({}),
    report(report) {
      reports.push(report);
      return Promise.resolve({});
    },
    heard: () => Promise.resolve(),
    said: () => Promise.resolve([]),
  };
  const kept = [];
  const calls = {
    state: () => Promise.resolve(account()),
    content: async (bytes) => {
      kept.push(bytes);
      return `tilo.content:${HASH}`;
    },
    do: () => Promise.resolve({}),
  };
  const gateway = {
    user: { id: "900" },
    joinVoice: (guild, channel) =>
      Promise.resolve({ guild, channel, endpoint: "c-sjc01.discord.media", token: "t", session: "s" }),
    leaveVoice: () => {},
  };
  const picture = pictureFetch();
  const carried = startCalls({
    calls,
    lane,
    gateway,
    makeVoice: async () => ({ said: [], muted: false, say() {}, close() {} }),
    fetch: picture.fetch,
    content: (bytes) => calls.content(bytes),
    log: () => {},
  });
  try {
    await carried.start();
    carried.hear({
      state: { guild: "123", user: "7001", channel: "458", name: "Ada", avatar: "9f3" },
    });
    await until(
      () => reports.some((report) => report.calls?.[0]?.people?.[0]?.avatar === HASH),
      "the report with the avatar's hash",
    );
    const report = reports.find((entry) => entry.calls?.[0]?.people?.[0]?.avatar === HASH);
    expect(report.calls[0].call).toBe("458");
    expect(report.calls[0].people[0]).toMatchObject({
      account: "7001",
      name: "Ada",
      avatar_url: AVATAR,
      avatar: HASH,
    });
    expect(picture.asked).toEqual([AVATAR]);
    expect(kept).toHaveLength(1);
    expect(voicePairs(account().accounts[0])).toEqual([{ discord: "458", tilo: "voice-tl" }]);
  } finally {
    carried.close();
  }
});
