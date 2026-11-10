/** Keys: the account's, the ones minted for a workspace, and asking for one. */

import { describe, expect, it } from "bun:test";

import { AskPending, Refused, Tilo } from "../src/index.ts";
import { FakeDoor, WORKSPACE_ID } from "./fakeDoor.ts";

function open(house: FakeDoor): Tilo {
  return new Tilo({ key: house.key, door: "https://door.test", fetch: house.fetch });
}

describe("keys", () => {
  it("lists this key and the keys it minted", async () => {
    const house = new FakeDoor();
    const tilo = open(house);

    const keys = await tilo.keys.list();

    expect(keys.map((key) => key.grant)).toEqual(["g-root"]);
    expect(keys[0]?.scope).toBe("account");
    expect(house.asked("tilo_do").at(-1)?.arguments.path).toBe("account/keys#list");
    await tilo.close();
  });

  it("mints a key for one workspace", async () => {
    const house = new FakeDoor();
    const tilo = open(house);

    const minted = await tilo.keys.mint(WORKSPACE_ID, "jane's agent");

    expect(minted.token).toBe("tilo_sk_minted");
    expect(minted.workspace).toBe(WORKSPACE_ID);
    expect(minted.mcpUrl.endsWith(`/w/${WORKSPACE_ID}/mcp`)).toBe(true);
    expect(house.asked("tilo_do").at(-1)?.arguments.args).toEqual({
      workspace: WORKSPACE_ID,
      name: "jane's agent",
    });
    await tilo.close();
  });

  it("mints from the workspace itself", async () => {
    const tilo = open(new FakeDoor());

    const minted = await tilo.workspace("jane-doe").mintMcp(null, 3600);

    expect(minted.workspace).toBe(WORKSPACE_ID);
    expect(minted.grant).toBe("g-minted");
    await tilo.close();
  });

  it("revokes a key and answers which", async () => {
    const tilo = open(new FakeDoor());

    expect(await tilo.keys.revoke("g-root")).toEqual({ revoked: "g-root" });
    await tilo.close();
  });

  it("is refused when deleting a workspace", async () => {
    const tilo = open(new FakeDoor());

    const failure = tilo.workspaces.delete("jane-doe");

    await expect(failure).rejects.toBeInstanceOf(Refused);
    await expect(failure).rejects.toMatchObject({
      code: "not_supported",
      nextSteps: ["revoke the keys that reach it with account/keys#revoke"],
    });
    await tilo.close();
  });

  it("raises the ask when a key is requested and answers it later", async () => {
    const house = new FakeDoor();
    const tilo = open(house);

    let ask: AskPending | null = null;
    try {
      await tilo.keys.create("visa-backend", WORKSPACE_ID);
    } catch (error) {
      ask = error as AskPending;
    }

    expect(ask?.url).toBe("https://mcp.tilo.sh/keys/approve/abc");
    expect(ask?.invocationId).toBe("key-abc");

    house.asks["key-abc"] = { status: "approved" };
    const key = (await ask?.wait()) as { token: string; grant: string; scope: string };
    expect(key.token).toBe("tilo_sk_created");
    expect(key.grant).toBe("g-created");
    expect(key.scope).toBe("account");
    await tilo.close();
  });

  it("shows a link and polls for a terminal key", async () => {
    const house = new FakeDoor();
    const tilo = open(house);

    const signIn = await tilo.signIn("cli");
    expect(signIn.url).toBe("https://mcp.tilo.sh/keys/approve/abc");
    expect(house.newKeyAsked).toEqual({ name: "cli" });

    house.polledKey = { key: "tilo_sk_new", grant: "g-new", name: "cli", scope: "account" };
    const key = await signIn.wait();

    expect(key.token).toBe("tilo_sk_new");
    expect(house.polls).toBeGreaterThanOrEqual(1);
    await tilo.close();
  });

  it("invites another person into a workspace with a link", async () => {
    const house = new FakeDoor();
    const tilo = open(house);

    const invitation = await tilo.workspace("jane-doe").members.invite({ role: "editor" });

    expect(invitation.link.startsWith("https://tilo.sh/i/")).toBe(true);
    expect(invitation.role).toBe("editor");
    expect(invitation.sent).toBe(false);
    expect(house.asked("tilo_do").at(-1)?.arguments.path).toBe("workspace/members#invite");
    expect(house.asked("tilo_do").at(-1)?.arguments.args).toEqual({ role: "editor" });
    await tilo.close();
  });

  it("says an emailed invitation was not sent, and lists the members", async () => {
    const house = new FakeDoor();
    const tilo = open(house);
    const workspace = tilo.workspace("jane-doe");

    const invitation = await workspace.members.invite({ email: "rosa@example.com" });
    expect(invitation.role).toBe("viewer");
    expect(invitation.email).toBe("rosa@example.com");
    expect(invitation.next).not.toBeNull();

    const members = await workspace.members.list();
    expect(members.map((member) => [member.name, member.role])).toEqual([["Rosa Ortiz", "editor"]]);
    await tilo.close();
  });

  it("reaches a workspace one was invited into by its id", async () => {
    const house = new FakeDoor();
    const tilo = open(house);
    const invited = "abcdef01-2345-6789-abcd-ef0123456789";

    expect((await tilo.workspace(invited).info()).id).toBe(invited);
    await tilo.close();
  });
});
