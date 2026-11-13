/**
 * Against a real API server: `TILO_TEST_API_URL` and `TILO_TEST_KEY` name one.
 *
 * The server is the local harness (`api-on-relay`); the key reaches every
 * workspace of its person. Nothing here fakes the wire.
 */

import { describe, expect, it } from "bun:test";

import { AskPending, KeyRevoked, Tilo } from "../src/index.ts";

const url = process.env.TILO_TEST_API_URL;
const key = process.env.TILO_TEST_KEY;
const skip = url === undefined || key === undefined;
const name = (prefix: string) => `${prefix}-${crypto.randomUUID().slice(0, 8)}`;

describe.skipIf(skip)("a real API server", () => {
  function open(): Tilo {
    return new Tilo({ key: key as string, url: url as string });
  }

  it("says its version, and makes, lists and reads a workspace", async () => {
    const tilo = open();
    expect((await tilo.info()).apiVersion).toBe("1");

    const created = await tilo.workspaces.create(name("sdk-test"));
    const listing = await tilo.workspaces.list();
    expect(listing.named(created.name)).toBeDefined();

    const apps = await tilo.workspace(created.id).ls();
    expect(apps.path).toBe("apps");
    tilo.close();
  });

  it("uploads a file and answers its reference", async () => {
    const tilo = open();
    const created = await tilo.workspaces.create(name("sdk-upload"));

    const content = await tilo
      .workspace(created.id)
      .files.upload(new TextEncoder().encode("a passport, in bytes"));

    expect(content.startsWith("tilo.content:")).toBe(true);
    tilo.close();
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
    tilo.close();
  });

  it("answers a watch as a stream, with cursors", async () => {
    const tilo = open();
    const created = await tilo.workspaces.create(name("sdk-watch"));
    const workspace = tilo.workspace(created.id);

    const watching = (async () => {
      for await (const event of workspace.watch()) {
        return event;
      }
      return null;
    })();
    await workspace.ls();
    await workspace.do("notes#create-page", { title: "Arrivals", markdown: "- 09:40 Ada\n" });

    const event = await Promise.race([
      watching,
      new Promise<never>((_, reject) =>
        setTimeout(() => reject(new Error("no event in 30s")), 30_000),
      ),
    ]);
    expect(typeof event?.kind).toBe("string");
    expect(typeof event?.cursor).toBe("string");
    tilo.close();
  });

  it("deletes a workspace, and its minted key with it", async () => {
    const tilo = open();
    const created = await tilo.workspaces.create(name("sdk-delete"));
    const minted = await tilo.keys.mint(created.id);

    const answer = await tilo.workspaces.delete(created.id);

    expect(answer.deleted).toBe(created.id);
    expect(answer.revoked_keys).toContain(minted.grant);
    const listed = await tilo.workspaces.list();
    expect(listed.workspaces.map((workspace) => workspace.id)).not.toContain(created.id);

    const agent = new Tilo({ key: minted.token, url });
    await expect(agent.workspace(created.id).ls()).rejects.toBeInstanceOf(KeyRevoked);
    agent.close();

    await expect(tilo.workspaces.delete(created.id)).rejects.toMatchObject({ code: "not_found" });
    tilo.close();
  });

  it("asks the person for a key and names the link", async () => {
    const tilo = open();

    const failure = tilo.keys.create(name("sdk-ask"));

    await expect(failure).rejects.toBeInstanceOf(AskPending);
    tilo.close();
  });
});
