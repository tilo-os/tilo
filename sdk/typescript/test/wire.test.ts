/** What each request the SDK makes says: session, typed answers, run, errors. */

import { describe, expect, it } from "bun:test";

import { ProtocolError, RateLimited, Refused, Tilo } from "../src/index.ts";
import { FakeDoor, WORKSPACE_ID } from "./fakeDoor.ts";

function client(door: FakeDoor, key = door.key): Tilo {
  return new Tilo({ key, door: "https://door.test", fetch: door.fetch });
}

describe("the wire", () => {
  it("asks a busy host again until it answers", async () => {
    const door = new FakeDoor();
    door.busy = 2;
    const tilo = client(door);

    const created = await tilo.workspaces.create("Applicant: Jane Doe");

    expect(created.name).toBe("Applicant: Jane Doe");
    expect(door.busy).toBe(0);
    // The refused calls ran nothing: the door saw the create once.
    expect(door.calls.filter((call) => call.tool === "tilo_do").length).toBe(1);
  });

  it("asks a busy workspace entry again", async () => {
    const door = new FakeDoor();
    door.busyEntering = 2;
    const tilo = client(door);

    await tilo.workspace("jane-doe").ls();

    expect(door.busyEntering).toBe(0);
    expect(door.calls.filter((call) => call.tool === "tilo_ls").length).toBe(1);
  });

  it("throws RateLimited once the host stays busy past the deadline", async () => {
    const door = new FakeDoor();
    door.busy = 1000;
    door.busyWaitMs = 20;
    const tilo = new Tilo({
      key: door.key,
      door: "https://door.test",
      fetch: door.fetch,
      retryDeadlineMs: 100,
    });

    const refused = await tilo.workspaces.create("Applicant: Jane Doe").catch((error) => error);

    expect(refused).toBeInstanceOf(RateLimited);
    expect(refused).toBeInstanceOf(Refused);
    expect(refused.code).toBe("rate_limited");
    expect(refused.retryAfterMs).toBe(20);
    expect(door.busy).toBeLessThan(1000);
  });

  it("opens a session and asks for typed answers", async () => {
    const door = new FakeDoor();
    const tilo = client(door);

    const listing = await tilo.workspaces.list();

    expect(listing.workspaces.map((workspace) => workspace.name)).toEqual([
      "jane-doe",
      "john-doe",
      "applicant-jane-doe",
    ]);
    expect(listing.workspaces.map((workspace) => workspace.role)).toEqual([
      "owner",
      "owner",
      "editor",
    ]);
    expect(listing.named("applicant-jane-doe")?.role).toBe("editor");
    expect(listing.hosts[0]?.name).toBe("eu1");
    expect(door.opened).toBeGreaterThanOrEqual(1);
    const call = door.asked("tilo_do").at(-1);
    expect(call?.arguments.path).toBe("account/workspaces#list");
    expect(call?.structured).toBe(true);
    expect(call?.workspace).toBeNull();
    await tilo.close();
  });

  it("sends a workspace's calls to its own session", async () => {
    const door = new FakeDoor();
    const tilo = client(door);

    await tilo.workspace("jane-doe").ls();

    const call = door.asked("tilo_ls").at(-1);
    expect(call?.workspace).toBe(WORKSPACE_ID);
    expect(call?.arguments).toEqual({});
    await tilo.close();
  });

  it("reads an event stream reply to its last message", async () => {
    const door = new FakeDoor();
    door.stream = true;
    const tilo = client(door);

    const listing = await tilo.workspace("jane-doe").ls();

    expect(listing.path).toBe("apps");
    expect(listing.rows.map((row) => row.name)).toEqual(["notes", "pdf-viewer", "terminal"]);
    await tilo.close();
  });

  it("opens a session again when the door forgot it", async () => {
    const door = new FakeDoor();
    const tilo = client(door);
    const workspace = tilo.workspace("jane-doe");
    await workspace.ls();
    const opened = door.opened;
    door.sessions.clear();

    const listing = await workspace.ls("notes");

    expect(listing.path).toBe("notes");
    expect(door.opened).toBe(opened + 1);
    await tilo.close();
  });

  it("says the key is the problem when the door answers 401", async () => {
    const door = new FakeDoor();
    const tilo = client(door, "tilo_sk_other");

    const failure = tilo.workspaces.list();
    await expect(failure).rejects.toThrow(ProtocolError);
    await expect(failure).rejects.toThrow(/key/);
    await tilo.close();
  });

  it("is a protocol error when a call answers text", async () => {
    const door = new FakeDoor();
    const tilo = client(door);
    (door as unknown as { tool: () => null }).tool = () => null;

    const failure = tilo.workspace("jane-doe").call("tilo_nope", {});

    await expect(failure).rejects.toThrow(/typed/);
    await tilo.close();
  });

  it("drops the catalog when a workspace is created", async () => {
    const door = new FakeDoor();
    const tilo = client(door);
    await tilo.workspaces.list();
    expect(tilo.catalog).not.toBeNull();

    const created = await tilo.workspaces.create("new-applicant");

    expect(created.name).toBe("new-applicant");
    expect(tilo.catalog).toBeNull();
    expect(door.workspaces.at(-1)?.id).toBe(created.id);
    await tilo.close();
  });
});
