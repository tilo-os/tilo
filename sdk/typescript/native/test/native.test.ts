/**
 * The native part against a real Tilo host: `TILO_KEY_FILE` names a key file
 * of a device of the person (the `api-on-relay` harness writes one).
 *
 * Nothing stands between the test and the host: the addon is the API in
 * process with the device's own key, dialling the relay as the person's
 * devices do. Without `TILO_KEY_FILE` this is skipped; with it, a missing
 * binary fails.
 */
import { describe, expect, setDefaultTimeout, test } from "bun:test";
import { readFileSync } from "node:fs";
import { Host, open } from "../index.js";

const KEY_FILE = process.env.TILO_KEY_FILE;
const JSON_TYPE = { "content-type": "application/json" };

async function whole(
  host: Host,
  method: string,
  path: string,
  body?: unknown,
  headers: Record<string, string> = {},
): Promise<{ status: number; headers: Record<string, string>; json: any }> {
  const payload = body === undefined ? new Uint8Array() : new TextEncoder().encode(JSON.stringify(body));
  const reply = await host.request(method, path, { ...JSON_TYPE, ...headers }, payload);
  if (!(reply.body instanceof Uint8Array)) throw new Error(`${path} answered a stream`);
  const text = new TextDecoder().decode(reply.body);
  return { status: reply.status, headers: reply.headers, json: text ? JSON.parse(text) : null };
}

// A host enters a workspace and starts its apps on the first call: seconds, not milliseconds.
setDefaultTimeout(60_000);

describe.skipIf(!KEY_FILE)("the native part on a real host", () => {
  const host = () => new Host(KEY_FILE as string);

  test("the key reaches the host as the person", async () => {
    const { status, headers, json } = await whole(host(), "GET", "/v1");
    expect(status).toBe(200);
    expect(headers["content-type"]).toStartWith("application/json");
    expect(json.key.scope).toBe("all");
  });

  test("a key file is also its contents", async () => {
    const contents = JSON.parse(readFileSync(KEY_FILE as string, "utf8"));
    const { status } = await whole(new Host(contents), "GET", "/v1");
    expect(status).toBe(200);
  });

  test("open() takes the key file or the key and its certificate, as tilo-sdk opens a host", async () => {
    const contents = JSON.parse(readFileSync(KEY_FILE as string, "utf8"));
    const fromFile = await open({ url: contents.host, keyFile: KEY_FILE as string });
    expect((await whole(fromFile, "GET", "/v1")).status).toBe(200);
    const fromKey = await open({ url: contents.host, key: contents.key, cert: contents.cert });
    expect((await whole(fromKey, "GET", "/v1")).status).toBe(200);
  });

  test("a bad key file is refused", () => {
    expect(() => new Host({ host: "http://127.0.0.1:1", name: "x", key: "zz", cert: "" })).toThrow();
  });

  test("a workspace is made once and its apps answer", async () => {
    const h = host();
    const name = `native-${crypto.randomUUID().slice(0, 8)}`;
    const retried = { "idempotency-key": name };
    const made = await whole(h, "POST", "/v1/workspaces", { name }, retried);
    expect(made.status).toBe(201);
    const again = await whole(h, "POST", "/v1/workspaces", { name }, retried);
    expect(again.json.id).toBe(made.json.id);

    const listed = await whole(h, "GET", "/v1/workspaces");
    expect(listed.status).toBe(200);
    expect(JSON.stringify(listed.json)).toContain(made.json.id);

    const workspace = made.json.id;
    const opened = await whole(h, "POST", `/v1/workspaces/${workspace}/resources/notes/actions/open`, {});
    expect(opened.status).toBe(200);
    const apps = await whole(h, "GET", `/v1/workspaces/${workspace}/resources`);
    expect(apps.status).toBe(200);
    expect(apps.json.rows.length).toBeGreaterThan(0);
  });

  test("a workspace's events stream as they happen", async () => {
    const h = host();
    const made = await whole(h, "POST", "/v1/workspaces", { name: "native-events" });
    const workspace = made.json.id;
    const reply = await h.request("GET", `/v1/workspaces/${workspace}/events`, {}, new Uint8Array());
    expect(reply.status).toBe(200);
    expect(reply.headers["content-type"]).toStartWith("text/event-stream");
    if (reply.body instanceof Uint8Array) throw new Error("the events answered whole");
    const events = reply.body;

    const firstEvent = (async () => {
      let seen = "";
      const decoder = new TextDecoder();
      for await (const chunk of events) {
        expect(chunk).toBeInstanceOf(Uint8Array);
        seen += decoder.decode(chunk, { stream: true });
        if (seen.includes("\n\n") && seen.includes("event:")) return seen;
      }
      throw new Error(`the stream ended after ${JSON.stringify(seen)}`);
    })();
    const opened = await whole(h, "POST", `/v1/workspaces/${workspace}/resources/notes/actions/open`, {});
    expect(opened.status).toBe(200);
    expect(await firstEvent).toContain("data:");
  }, 60_000);
});
