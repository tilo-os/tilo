/** The account: the API it calls, its workspaces and its keys. */

import { describe, expect, it } from "bun:test";

import {
  AskPending,
  KeyExpired,
  KeyRevoked,
  Refused,
  Tilo,
  keyFrom,
  urlFrom,
} from "../src/index.ts";
import { FakeApi, WORKSPACE_ID } from "./fakeApi.ts";

function open(api: FakeApi): Tilo {
  return new Tilo({ url: "https://api.test", key: api.key, fetch: api.fetch });
}

describe("where a client calls", () => {
  it("names the API server and the key, the environment when not given", () => {
    const url = process.env.TILO_API_URL;
    const key = process.env.TILO_API_KEY;
    try {
      process.env.TILO_API_URL = "https://eu1.tilo.sh/";
      process.env.TILO_API_KEY = "tilo_sk_env";
      expect(urlFrom()).toBe("https://eu1.tilo.sh");
      expect(keyFrom()).toBe("tilo_sk_env");
      expect(urlFrom("https://api.example")).toBe("https://api.example");
      expect(keyFrom("tilo_sk_named")).toBe("tilo_sk_named");
    } finally {
      if (url === undefined) delete process.env.TILO_API_URL;
      else process.env.TILO_API_URL = url;
      if (key === undefined) delete process.env.TILO_API_KEY;
      else process.env.TILO_API_KEY = key;
    }
    expect(urlFrom(null)).toMatch(/^https:\/\//);
  });
});

describe("the account", () => {
  it("answers the API's version and what the key reaches", async () => {
    const api = new FakeApi();
    const info = await open(api).info();

    expect(info.apiVersion).toBe("1");
    expect(info.server).toBe("tilo-api test");
    expect(info.key?.grant).toBe("g-root");
    expect(api.last()?.path).toBe("/v1");
  });

  it("lists the person's workspaces, and names one by its name", async () => {
    const api = new FakeApi();
    const tilo = open(api);

    const listing = await tilo.workspaces.list();

    expect(listing.workspaces.map((workspace) => workspace.name)).toEqual([
      "jane-doe",
      "john-doe",
      "applicant-jane-doe",
    ]);
    expect(listing.named("jane-doe")?.id).toBe(WORKSPACE_ID);
    expect(listing.named(WORKSPACE_ID)?.host).toBe("eu1");
    expect(tilo.workspaces.named("JANE-DOE")?.id).toBe(WORKSPACE_ID);
    expect(api.last()?.path).toBe("/v1/workspaces");
  });

  it("makes a workspace and then lists it", async () => {
    const api = new FakeApi();
    const tilo = open(api);

    const created = await tilo.workspaces.create("Applicant: Jane Doe");

    expect(created.name).toBe("Applicant: Jane Doe");
    expect(created.role).toBe("owner");
    expect(api.last()?.body).toBe('{"name":"Applicant: Jane Doe"}');
    expect((await tilo.workspaces.list()).named(created.id)).toBeDefined();
  });

  it("deletes a workspace, and says which keys went with it", async () => {
    const api = new FakeApi();
    const tilo = open(api);

    const deleted = await tilo.workspaces.delete(WORKSPACE_ID);

    expect(deleted.deleted).toBe(WORKSPACE_ID);
    expect(deleted.revoked_keys).toEqual(["g-minted"]);
    expect(api.last()?.method).toBe("DELETE");
    expect((await tilo.workspaces.list()).named(WORKSPACE_ID)).toBeUndefined();
  });

  it("answers the person's usage, and the VM time no workspace ran", async () => {
    const api = new FakeApi();
    const usage = await open(api).usage();

    expect(usage.plan).toBe("alpha");
    expect(usage.vmSeconds).toBe(0);
    expect(usage.running).toEqual([]);
    expect(api.last()?.path).toBe("/v1/account");
  });
});

describe("keys", () => {
  it("lists this key and the keys it minted", async () => {
    const api = new FakeApi();
    const keys = await open(api).keys.list();

    expect(keys.map((key) => key.grant)).toEqual(["g-root"]);
    expect(keys[0]?.scope).toBe("all");
    expect(keys[0]?.browserPages).toBe(true);
    expect(api.last()?.path).toBe("/v1/keys");
  });

  it("mints a key for one workspace", async () => {
    const api = new FakeApi();
    const minted = await open(api).keys.mint(WORKSPACE_ID, "jane's agent", 3600);

    expect(minted.token).toBe("tilo_sk_minted");
    expect(minted.workspace).toBe(WORKSPACE_ID);
    expect(minted.mcpUrl.endsWith(`/w/${WORKSPACE_ID}/mcp`)).toBe(true);
    expect(api.last()?.body).toBe(
      `{"workspace":"${WORKSPACE_ID}","name":"jane's agent","expires_in":3600}`,
    );
  });

  it("mints from the workspace itself", async () => {
    const api = new FakeApi();
    const minted = await open(api).workspace("jane-doe").keys.mint(null, 3600);

    expect(minted.grant).toBe("g-minted");
    expect(minted.workspace).toBe(WORKSPACE_ID);
  });

  it("revokes a key", async () => {
    const api = new FakeApi();
    expect(await open(api).keys.revoke("g-root")).toEqual({ revoked: "g-root" });
    expect(api.last()?.method).toBe("DELETE");
    expect(api.last()?.path).toBe("/v1/keys/g-root");
  });

  it("asks the person for a key, and answers it once they approve", async () => {
    const api = new FakeApi();
    api.pollsUntilApproved = 3;
    const tilo = open(api);

    const failure = tilo.keys.create("Nightly job", null, 3600);

    await expect(failure).rejects.toBeInstanceOf(AskPending);
    const ask = (await failure.catch((error) => error)) as AskPending;
    expect(ask.url).toBe("https://api.test/keys/approve/abc");
    expect(ask.invocationId).toBe("abc");
    expect(api.newKeyAsked).toEqual({ name: "Nightly job", expires_in: 3600 });

    const key = (await ask.wait()) as { token: string; name: string };
    expect(key.token).toBe("tilo_sk_created");
    expect(key.name).toBe("Nightly job");
    expect(api.polls).toBe(3);
  });

  it("signs a client in with the link it shows", async () => {
    const api = new FakeApi();
    const signIn = await open(api).signIn("Backend", WORKSPACE_ID, null, false);

    expect(signIn.url).toBe("https://api.test/keys/approve/abc");
    expect(api.newKeyAsked).toEqual({ name: "Backend", workspace: WORKSPACE_ID, browser_pages: false });
    expect((await signIn.wait()).grant).toBe("g-created");
  });
});

describe("a key turned away", () => {
  it("throws KeyRevoked with when it was revoked", async () => {
    const api = new FakeApi();

    const failure = new Tilo({ url: "https://api.test", key: "tilo_sk_revoked", fetch: api.fetch })
      .workspaces.list();

    await expect(failure).rejects.toBeInstanceOf(KeyRevoked);
    const refused = (await failure.catch((error) => error)) as KeyRevoked;
    expect(refused.revokedAtMs).toBe(1_794_000_000_000);
    expect(refused.nextSteps).toEqual(["ask the person for a new key"]);
  });

  it("throws KeyExpired for a key past its expiry", async () => {
    const api = new FakeApi();
    const failure = new Tilo({ url: "https://api.test", key: "tilo_sk_expired", fetch: api.fetch })
      .keys.list();

    await expect(failure).rejects.toBeInstanceOf(KeyExpired);
  });
});

describe("a workspace named by its name", () => {
  it("is resolved from the account's listing, once", async () => {
    const api = new FakeApi();
    const tilo = open(api);
    const workspace = tilo.workspace("jane-doe");

    expect(await workspace.id()).toBe(WORKSPACE_ID);
    expect((await workspace.info()).role).toBe("owner");
    expect(api.asked("GET", "/v1/workspaces")).toHaveLength(1);
  });

  it("throws Refused for a name the account does not list", async () => {
    const api = new FakeApi();
    await expect(open(api).workspace("mallory").id()).rejects.toBeInstanceOf(Refused);
  });
});
