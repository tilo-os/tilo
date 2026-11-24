/** The key carrier: MCP `tools/call` over HTTPS, its session, and its refusals. */

import { describe, expect, it } from "bun:test";

import { type FetchLike, KeyRevoked, Refused, Tilo } from "../src/index.ts";

const WORKSPACE = "6f9619ff-8b86-d011-b42d-00c04fc964ff";

interface Asked {
  url: string;
  headers: Record<string, string>;
  body: {
    method: string;
    params?: {
      name?: string;
      arguments?: Record<string, unknown>;
      _meta?: Record<string, unknown>;
    };
  };
}

/** A fake MCP server: records each request, and answers what the test tells it to. */
class FakeServer {
  readonly asked: Asked[] = [];
  reply: (body: Asked["body"]) => Response = () => toolAnswer({});

  fetch: FetchLike = (async (input: Parameters<FetchLike>[0], init?: Parameters<FetchLike>[1]) => {
    const body = JSON.parse(String(init?.body ?? "{}")) as Asked["body"];
    this.asked.push({
      url: String(input),
      headers: (init?.headers ?? {}) as Record<string, string>,
      body,
    });
    return this.reply(body);
  }) as unknown as FetchLike;
}

/** The answer a `tools/call` gives with `structuredContent` `answer`. */
function toolAnswer(answer: unknown): Response {
  return json({ jsonrpc: "2.0", id: 1, result: { structuredContent: answer } });
}

function opened(): Response {
  return new Response("", { status: 200, headers: { "mcp-session-id": "session-1" } });
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function client(server: FakeServer, retryDeadlineMs = 0): Tilo {
  return new Tilo({ key: "tilo_sk_test", url: "https://api.test/", fetch: server.fetch, retryDeadlineMs });
}

describe("a key's call", () => {
  it("opens one session and sends its tools with the workspace", async () => {
    const server = new FakeServer();
    server.reply = (body) =>
      body.method === "initialize" ? opened() : toolAnswer({ path: "apps", rows: [] });

    await client(server).workspace(WORKSPACE).ls();

    expect(server.asked).toHaveLength(2);
    const [initialize, call] = server.asked;
    expect(initialize?.body.method).toBe("initialize");
    expect(initialize?.headers["mcp-session-id"]).toBeUndefined();
    expect(call?.url).toBe("https://api.test/mcp");
    expect(call?.headers.authorization).toBe("Bearer tilo_sk_test");
    expect(call?.headers["mcp-session-id"]).toBe("session-1");
    expect(call?.body.params?.name).toBe("tilo_ls");
    expect(call?.body.params?.arguments?.workspace).toBe(WORKSPACE);
    expect(call?.body.params?._meta).toEqual({ "so.tilo.mcp": { structured: true } });
  });

  it("names no workspace for the account's own calls", async () => {
    const server = new FakeServer();
    server.reply = (body) =>
      body.method === "initialize" ? opened() : toolAnswer({ status: "ok", result: { workspaces: [] } });

    await client(server).workspaces.list();

    const call = server.asked.at(-1);
    expect(call?.body.params?.name).toBe("tilo_do");
    expect(call?.body.params?.arguments?.path).toBe("account/workspaces#list");
    expect(call?.body.params?.arguments?.workspace).toBeUndefined();
  });

  it("sends the invocation group of the calls inside it", async () => {
    const server = new FakeServer();
    server.reply = (body) =>
      body.method === "initialize" ? opened() : toolAnswer({ path: "apps", rows: [] });
    const workspace = client(server).workspace(WORKSPACE);

    await workspace.invocation("Import the backlog", async () => {
      await workspace.ls();
    });

    const meta = server.asked.at(-1)?.body.params?._meta as
      | { "so.tilo.mcp"?: { invocation?: { id?: string; title?: string } } }
      | undefined;
    expect(meta?.["so.tilo.mcp"]?.invocation?.title).toBe("Import the backlog");
    expect(meta?.["so.tilo.mcp"]?.invocation?.id).toMatch(/^sdk:/);
  });

  it("throws the refusal a failed tool answered", async () => {
    const server = new FakeServer();
    server.reply = (body) =>
      body.method === "initialize"
        ? opened()
        : json({
            jsonrpc: "2.0",
            id: 1,
            result: {
              isError: true,
              structuredContent: {
                error: { code: "not_permitted", message: "no" },
                action: { id: "list" },
              },
            },
          });

    const failure = (await client(server).workspaces.list().catch((error: unknown) => error)) as Refused;

    expect(failure).toBeInstanceOf(Refused);
    expect(failure.code).toBe("not_permitted");
    expect(failure.action).toEqual({ id: "list" });
  });

  it("throws KeyRevoked for a key the server turned away", async () => {
    const server = new FakeServer();
    server.reply = (body) =>
      body.method === "initialize"
        ? opened()
        : json({ error: { code: "key_revoked", message: "revoked", revoked_at_ms: 1_794_000_000_000 } }, 401);

    const failure = (await client(server).keys.list().catch((error: unknown) => error)) as KeyRevoked;

    expect(failure).toBeInstanceOf(KeyRevoked);
    expect(failure.revokedAtMs).toBe(1_794_000_000_000);
  });

  it("asks again while the server is briefly unavailable", async () => {
    const server = new FakeServer();
    let calls = 0;
    server.reply = (body) => {
      if (body.method === "initialize") {
        return opened();
      }
      calls += 1;
      return calls === 1 ? new Response("", { status: 503 }) : toolAnswer({ status: "ok", result: { workspaces: [] } });
    };

    const listing = await client(server, 5_000).workspaces.list();

    expect(calls).toBe(2);
    expect(listing.workspaces).toEqual([]);
  });
});
