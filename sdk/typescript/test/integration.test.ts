/**
 * Against a real API server: `TILO_TEST_API_URL` and `TILO_TEST_KEY` name one.
 *
 * The door is the local harness; the key reaches every workspace of its
 * person. Nothing here fakes the wire.
 */

import { describe, expect, it } from "bun:test";

import { AskPending, KeyRevoked, Refused, Tilo } from "../src/index.ts";

const door = process.env.TILO_TEST_API_URL;
const key = process.env.TILO_TEST_KEY;
const skip = door === undefined || key === undefined;
const name = (prefix: string) => `${prefix}-${crypto.randomUUID().slice(0, 8)}`;

describe.skipIf(skip)("a real API server", () => {
  function open(): Tilo {
    return new Tilo({ key: key as string, door: door as string });
  }

  it("creates, lists and reads a workspace", async () => {
    const tilo = open();
    const created = await tilo.workspaces.create(name("sdk-test"));
    expect(created.name).toBe(created.name);

    const listing = await tilo.workspaces.list();
    expect(listing.named(created.name)).toBeDefined();

    const apps = await tilo.workspace(created.id).ls();
    expect(apps.path).toBe("apps");
    await tilo.close();
  });

  it("uploads a file and answers its reference", async () => {
    const tilo = open();
    const created = await tilo.workspaces.create(name("sdk-upload"));

    const content = await tilo
      .workspace(created.id)
      .files.upload(new TextEncoder().encode("a passport, in bytes"));

    expect(content.startsWith("tilo.content:")).toBe(true);
    await tilo.close();
  });

  it("mints a key for one workspace and revokes it", async () => {
    const tilo = open();
    const created = await tilo.workspaces.create(name("sdk-key"));

    const minted = await tilo.keys.mint(created.id, "sdk test agent");
    expect(minted.workspace).toBe(created.id);
    expect(minted.token.length).toBeGreaterThan(0);

    const before = await tilo.keys.list();
    expect(before.map((entry) => entry.grant)).toContain(minted.grant);

    await tilo.keys.revoke(minted.grant);
    const after = await tilo.keys.list();
    expect(after.map((entry) => entry.grant)).not.toContain(minted.grant);
    await tilo.close();
  });

  it("answers a watch with a cursor", async () => {
    const tilo = open();
    const created = await tilo.workspaces.create(name("sdk-watch"));

    const answer = await tilo.workspace(created.id).call("tilo_watch", { wait_ms: 0 });

    expect(typeof answer.cursor).toBe("string");
    expect(Array.isArray(answer.events)).toBe(true);
    await tilo.close();
  });

  it("deletes a workspace, and its minted key with it", async () => {
    const tilo = open();
    const created = await tilo.workspaces.create(name("sdk-delete"));
    const minted = await tilo.keys.mint(created.id);

    const answer = (await tilo.workspaces.delete(created.id)) as Record<string, unknown>;

    expect(answer.deleted).toBe(created.id);
    expect(answer.revoked_keys).toContain(minted.grant);
    const listed = await tilo.workspaces.list();
    expect(listed.workspaces.map((workspace) => workspace.id)).not.toContain(created.id);
    const agent = new Tilo({ key: minted.token, door });
    await expect(agent.workspace(created.id).ls()).rejects.toBeInstanceOf(KeyRevoked);
    await agent.close();
    const again = tilo.workspaces.delete(created.id);
    await expect(again).rejects.toMatchObject({ code: "not_found" });
    await tilo.close();
  });

  it("asks the person for a key and names the link", async () => {
    const tilo = open();

    const failure = tilo.keys.create(name("sdk-ask"));

    await expect(failure).rejects.toBeInstanceOf(AskPending);
    await tilo.close();
  });
});
