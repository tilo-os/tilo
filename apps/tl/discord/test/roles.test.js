// The workspace's roles kept as the bridge's own Discord roles: made, corrected, denied what
// they may not do in each bridged channel, and held by linked accounts as their person's role.

import { test, expect } from "bun:test";
import { carryFromDiscord, keepRoles } from "../main.js";
import { memberRoles, rolesPlan, permissions, channelDeny } from "../src/roles.js";

const ADA = JSON.stringify({ member: { person: "ada" } });

/// The bridge's record, mapping roles, with one bridged channel and ada's account linked.
function account(overrides = {}) {
  return {
    guild_id: "123",
    token_item: "vault.item:discord-token",
    map_roles: true,
    map: [{ kind: "channel", tilo: "tl:so.tilo.chat:design", discord: "456" }],
    links: [{ discord_user: "3001", person: ADA }],
    ...overrides,
  };
}

/// What Discord showed: `@everyone`, one role of the server's own, and the bridged channel.
function layout(roles = [], overwrites = []) {
  return {
    roles: [{ id: "123", name: "@everyone", permissions: "0" }, { id: "9", name: "Mods" }, ...roles],
    channels: [{ id: "456", kind: "text", name: "design", overwrites }],
  };
}

/// A workspace the bridge acts in: the actions it calls, the requests Discord hears and their
/// answers, and the workspace's people.
function workspace({ people = [] } = {}) {
  const done = [];
  const requests = [];
  let next = 700;
  return {
    done,
    requests,
    calls: {
      async do(call) {
        done.push([call.action, call.args]);
        return {};
      },
      async webCall(request) {
        requests.push([request.method, request.url.replace(/^.*\/api\/v10/, ""), request.body ?? null]);
        return { status: 200, body: JSON.stringify({ id: String((next += 1)) }) };
      },
      async members() {
        return people;
      },
    },
  };
}

test("the workspace's roles are made in the server once, and each bridged channel denies what a Viewer and a Commenter may not do", async () => {
  const ws = workspace();
  const asked = new Set();
  await keepRoles({ calls: ws.calls, account: account(), layout: layout(), asked });

  const made = ws.requests.filter(([method, url]) => method === "POST" && url === "/guilds/123/roles");
  expect(made.map(([, , body]) => JSON.parse(body).name)).toEqual([
    "Tilo Viewer",
    "Tilo Commenter",
    "Tilo Editor",
    "Tilo Moderator",
    "Tilo Owner",
  ]);
  // Each is joined to its role here, through the bridge's guard first.
  expect(ws.done.filter(([action]) => action === "apply_role")).toHaveLength(5);
  expect(ws.done.filter(([action]) => action === "map").map(([, args]) => [args.tilo, args.discord])).toEqual([
    ["viewer", "701"],
    ["commenter", "702"],
    ["editor", "703"],
    ["moderator", "704"],
    ["owner", "705"],
  ]);
  // The Viewer's and the Commenter's roles are denied, in the one bridged channel, what they may
  // not do; the Commenter may still react.
  const overwrites = ws.requests.filter(([method]) => method === "PUT");
  expect(overwrites.map(([, url]) => url)).toEqual([
    "/channels/456/permissions/701",
    "/channels/456/permissions/702",
  ]);
  const viewer = JSON.parse(overwrites[0][2]);
  expect(viewer).toEqual({ allow: "0", deny: channelDeny("viewer").toString(), type: 0 });
  expect(BigInt(viewer.deny) & (1n << 11n)).toBe(1n << 11n);
  expect(BigInt(JSON.parse(overwrites[1][2]).deny) & (1n << 6n)).toBe(0n);

  // Before Discord echoes them, the next layout asks for nothing again.
  const mapped = account({
    map: [
      ...account().map,
      ...["viewer", "commenter", "editor", "moderator", "owner"].map((tilo, n) => ({
        kind: "role",
        tilo,
        discord: String(701 + n),
      })),
    ],
  });
  ws.requests.length = 0;
  await keepRoles({ calls: ws.calls, account: mapped, layout: layout(), asked });
  expect(ws.requests).toEqual([]);
});

test("a bridge role changed by hand in Discord is set back, and the server's own roles are left alone", () => {
  const state = account({ map: [...account().map, { kind: "role", tilo: "editor", discord: "703" }] });
  const right = { id: "703", name: "Tilo Editor", permissions: permissions("editor").toString(), color: 0x2ecc71 };
  const plan = rolesPlan(state, layout([{ ...right, name: "Editors!" }]));
  const editor = plan.apply.find((op) => op.tilo === "editor");
  expect(editor).toMatchObject({ id: "703", name: "Tilo Editor" });
  expect(plan.apply.some((op) => op.id === "9" || op.id === "123")).toBe(false);
  expect(rolesPlan(state, layout([right])).apply.some((op) => op.tilo === "editor")).toBe(false);

  // Before any layout, or for a bridge that leaves roles alone, nothing is planned.
  expect(rolesPlan(state, { roles: [], channels: [] }).apply).toEqual([]);
  expect(rolesPlan({ ...state, map_roles: false }, layout()).apply).toEqual([]);
  expect(rolesPlan({ ...state, only_channel: "456" }, layout()).apply).toEqual([]);
});

test("a linked member holds the bridge role of their role here, set back when changed by hand there", async () => {
  const roles = ["viewer", "commenter", "editor", "moderator", "owner"].map((tilo, n) => ({
    kind: "role",
    tilo,
    discord: String(701 + n),
  }));
  const state = account({ map: [...account().map, ...roles] });
  expect(memberRoles(state, "3001", "editor", ["703"])).toBeNull();
  expect(memberRoles(state, "3001", "editor", ["705", "9"])).toEqual({
    op: "set-member-roles",
    user: "3001",
    add: ["703"],
    remove: ["705"],
  });
  expect(memberRoles(state, "4004", "editor", [])).toBeNull();

  // Discord says someone gave ada's account the Owner's role: it goes back to her Editor's.
  const ws = workspace({ people: [{ person: "ada", role: "editor" }] });
  await carryFromDiscord({
    calls: ws.calls,
    account: state,
    event: {
      source: "MEMBER_CHANGED",
      payload: { guild_id: "123", user_id: "3001", roles: ["705"], banned: false, left: false, user: {} },
    },
    fetch: async () => ({ status: 404 }),
  });
  expect(ws.done.find(([action]) => action === "set_member_roles")?.[1].op).toMatchObject({
    add: ["703"],
    remove: ["705"],
  });
  expect(ws.requests.map(([method, url]) => [method, url])).toEqual([
    ["PUT", "/guilds/123/members/3001/roles/703"],
    ["DELETE", "/guilds/123/members/3001/roles/705"],
  ]);
});
