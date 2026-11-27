/** A key's client: `/v1` by URL, against a server that answers what each test tells it to. */

import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import { describe, expect, it } from "bun:test";

import { type FetchLike, Refused, Tilo, resourceProxy, run } from "../src/index.ts";

const WORKSPACE = "6f9619ff-8b86-d011-b42d-00c04fc964ff";
const BASE = `https://api.test/v1/workspaces/${WORKSPACE}`;

interface Asked {
  method: string;
  url: string;
  headers: Headers;
  body: Uint8Array;
}

/** A server: each request it was asked, and what it answers. */
class Server {
  readonly asked: Asked[] = [];
  reply: (asked: Asked) => Response = () => json({});

  fetch: FetchLike = (async (input: Parameters<FetchLike>[0], init?: Parameters<FetchLike>[1]) => {
    const asked: Asked = {
      method: init?.method ?? "GET",
      url: String(input),
      headers: new Headers(init?.headers),
      body: init?.body instanceof Uint8Array ? init.body : new Uint8Array(),
    };
    this.asked.push(asked);
    return this.reply(asked);
  }) as unknown as FetchLike;

  client(retryDeadlineMs = 0): Tilo {
    return new Tilo({ key: "tilo_sk_test", url: "https://api.test/", fetch: this.fetch, retryDeadlineMs });
  }
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function sent(asked: Asked | undefined): Record<string, unknown> {
  return JSON.parse(new TextDecoder().decode(asked?.body)) as Record<string, unknown>;
}

describe("a key's client", () => {
  it("lists by URL with the key", async () => {
    const server = new Server();
    server.reply = () => json({ path: "notes", rows: [] });

    const listing = await server.client().workspace(WORKSPACE).listResources({ path: "notes/Q3 plan" });

    expect(listing.rows).toEqual([]);
    const [asked] = server.asked;
    expect(asked?.method).toBe("GET");
    expect(asked?.url).toBe(`${BASE}/resources?path=notes%2FQ3+plan`);
    expect(asked?.headers.get("authorization")).toBe("Bearer tilo_sk_test");
    expect(asked?.headers.get("idempotency-key")).toBeNull();
  });

  it("runs an action in its group, and follows it until it finished", async () => {
    const server = new Server();
    server.reply = (asked) =>
      asked.method === "POST"
        ? json({ status: "pending", invocation: "inv-1" }, 202)
        : json({ status: "ok", invocation: "inv-1", result: { renamed: true }, link: "https://tilo.test/r/1" });
    const workspace = server.client().workspace(WORKSPACE);

    const group = await workspace.invocation("Tidy the plans", async (group) => {
      const answer = await run(workspace, { path: "notes/Q3/4 plan", action: "rename" }, { title: "Q4" });
      expect(answer.result).toEqual({ renamed: true });
      return group;
    });

    const [action, followed] = server.asked;
    expect(action?.url).toBe(`${BASE}/resources/notes/Q3/4%20plan/actions/rename`);
    expect(sent(action)).toEqual({ title: "Q4" });
    expect(action?.headers.get("idempotency-key")).not.toBeNull();
    expect(action?.headers.get("tilo-invocation")).toMatch(/^sdk:tidy-the-plans:/);
    expect(action?.headers.get("tilo-invocation-title")).toBe("Tidy the plans");
    expect(followed?.url).toBe(`${BASE}/invocations/inv-1?wait=60`);
    expect(followed?.headers.get("tilo-invocation")).toBe(group.id);
    expect(group.invocations).toEqual(["inv-1"]);
    expect(group.link).toBe("https://tilo.test/r/1");
  });

  it("asks the account again with the same key while its action waits", async () => {
    const server = new Server();
    let asked = 0;
    server.reply = () => {
      asked += 1;
      return asked < 3
        ? json({ status: "pending", invocation: "key-ask" }, 202)
        : json({ status: "ok", result: { key: "tilo_sk_new" } });
    };

    const answer = await run(server.client(), { path: "account/keys", action: "create" }, { name: "backend" });

    expect(answer.result).toEqual({ key: "tilo_sk_new" });
    expect(server.asked.map((each) => each.url)).toEqual(
      Array(3).fill("https://api.test/v1/resources/account/keys/actions/create"),
    );
    expect(new Set(server.asked.map((each) => each.headers.get("idempotency-key"))).size).toBe(1);
    expect(server.asked[0]?.headers.get("prefer")).toBe("wait=60");
  });

  it("throws the refusal it was answered", async () => {
    const server = new Server();
    server.reply = () =>
      json({ error: { code: "key_revoked", message: "revoked", revoked_at_ms: 1_794_000_000_000 } }, 401);

    const failure = await server.client().getApi().catch((error: unknown) => error);

    expect(failure).toBeInstanceOf(Refused);
    expect((failure as Refused).code).toBe("key_revoked");
    expect((failure as Refused).status).toBe(401);
    expect((failure as Refused).error.revoked_at_ms).toBe(1_794_000_000_000);
  });

  it("asks again while the server is briefly unavailable", async () => {
    const server = new Server();
    server.reply = () =>
      server.asked.length === 1
        ? json({ error: { code: "unavailable", message: "busy", retry_after_ms: 10 } }, 503)
        : json({ api_version: "1", server: "test" });

    const info = await server.client(5_000).getApi();

    expect(info.api_version).toBe("1");
    expect(server.asked).toHaveLength(2);
  });

  it("names the surface typed calls were made for", async () => {
    const server = new Server();
    server.reply = () => json({ status: "ok", result: null });
    const page = resourceProxy(server.client().workspace(WORKSPACE), "notes/Todo", "so.tilo.notes@0123456789abcdef", [
      "pin",
    ]);

    await (page.pin as () => Promise<unknown>)();

    expect(server.asked[0]?.url).toBe(`${BASE}/resources/notes/Todo/actions/pin`);
    expect(server.asked[0]?.headers.get("tilo-surface")).toBe("so.tilo.notes@0123456789abcdef");
  });

  it("sends a bulk call as a few group calls of one invocation", async () => {
    const server = new Server();
    server.reply = (asked) => json({ status: "ok", result: { applied: (sent(asked).actions as unknown[]).length } });
    const items = Array.from({ length: 1200 }, (_, index) => ({ title: `Issue ${index}` }));

    const answers = await server.client().workspace(WORKSPACE).resource("tasks/issues").many("create", items);

    expect(server.asked.map((asked) => asked.url)).toEqual(Array(3).fill(`${BASE}/resources/tasks/issues/actions/group`));
    const actions = server.asked.map((asked) => sent(asked).actions as unknown[]);
    expect(actions.map((each) => each.length)).toEqual([500, 500, 200]);
    expect(actions.flat()).toEqual(items.map((args) => ({ action: "create", args })));
    expect(new Set(server.asked.map((asked) => asked.headers.get("tilo-invocation"))).size).toBe(1);
    expect(server.asked[0]?.headers.get("tilo-invocation-title")).toBe("create (1200)");
    expect(answers.map((answer) => answer.result)).toEqual([{ applied: 500 }, { applied: 500 }, { applied: 200 }]);
  });

  it("puts a large file up in pieces at its BLAKE3 address", async () => {
    const server = new Server();
    const bytes = new Uint8Array(16 * 1024 * 1024 + 17).map((_, index) => index & 0xff);
    const hash = bytesToHex(blake3(bytes));
    server.reply = (asked) =>
      asked.headers.get("content-range")?.startsWith("bytes 0-")
        ? json({ offset: 16 * 1024 * 1024, size: bytes.length })
        : json({ offset: bytes.length, size: bytes.length, content: `tilo.content:${hash}` }, 201);

    const content = await server.client().workspace(WORKSPACE).upload(bytes);

    expect(content).toBe(`tilo.content:${hash}`);
    expect(server.asked.map((asked) => [asked.method, asked.url, asked.headers.get("content-range"), asked.body.length])).toEqual([
      ["PUT", `${BASE}/contents/${hash}`, `bytes 0-16777215/${bytes.length}`, 16 * 1024 * 1024],
      ["PUT", `${BASE}/contents/${hash}`, `bytes 16777216-16777232/${bytes.length}`, 17],
    ]);
  });

  it("reads a workspace's events, and ends their stream when its reader stops", async () => {
    const server = new Server();
    let cancelled = false;
    server.reply = () =>
      new Response(
        new ReadableStream<Uint8Array>({
          start(controller) {
            controller.enqueue(
              new TextEncoder().encode(
                ': keep-alive\n\nid: c1\nevent: change\ndata: {"path":"notes/Todo","what":"edited"}\n\n',
              ),
            );
          },
          cancel() {
            cancelled = true;
          },
        }),
        { headers: { "content-type": "text/event-stream" } },
      );
    const events = server.client().workspace(WORKSPACE).events({ path: "notes" });

    const first = await events.next();
    const waiting = events.next();
    await events.return?.();

    expect(first.value).toEqual({
      id: "c1",
      type: "change",
      workspace: WORKSPACE,
      data: { path: "notes/Todo", what: "edited" },
    });
    expect((await waiting).done).toBe(true);
    expect(cancelled).toBe(true);
    expect(server.asked[0]?.url).toBe(`${BASE}/events?path=notes`);
  });
});
