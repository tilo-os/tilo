/** The seam under every call: the request, its retries, and what an answer means. */

import { describe, expect, it } from "bun:test";

import {
  Endpoint,
  HttpTransport,
  NATIVE_PACKAGE,
  ProtocolError,
  RateLimited,
  Refused,
  Tilo,
} from "../src/index.ts";
import { FakeApi } from "./fakeApi.ts";

function open(api: FakeApi, retryDeadlineMs = 500): Endpoint {
  return new Endpoint(
    new HttpTransport("https://api.test", api.key, api.fetch),
    "11111111-2222-3333-4444-555555555555",
    retryDeadlineMs,
  );
}

describe("a request", () => {
  it("sends the key, the accept header and the user agent", async () => {
    const api = new FakeApi();
    await open(api).answer("GET", "/v1/workspaces", { idempotent: false });

    const call = api.last();
    expect(call?.method).toBe("GET");
    expect(call?.path).toBe("/v1/workspaces");
    expect(call?.headers.authorization).toBe(`Bearer ${api.key}`);
    expect(call?.headers.accept).toBe("application/json");
    expect(call?.headers["user-agent"]).toMatch(/^tilo-typescript\//);
  });

  it("sends an Idempotency-Key with every mutating call, the same one on a retry", async () => {
    const api = new FakeApi();
    api.busy = 1;
    await open(api).answer("POST", "/v1/workspaces", { json: { name: "w" } });

    const sent = api.asked("POST", "/v1/workspaces");
    expect(sent).toHaveLength(2);
    const first = sent[0]?.headers["idempotency-key"] ?? "";
    expect(first).not.toBe("");
    expect(sent[1]?.headers["idempotency-key"]).toBe(first);
    expect(sent[1]?.body).toBe('{"name":"w"}');
  });

  it("sends a new key for each logical call", async () => {
    const api = new FakeApi();
    const endpoint = open(api);
    await endpoint.answer("POST", "/v1/workspaces", { json: { name: "a" } });
    await endpoint.answer("POST", "/v1/workspaces", { json: { name: "b" } });

    const sent = api.asked("POST", "/v1/workspaces");
    expect(sent[0]?.headers["idempotency-key"]).not.toBe(sent[1]?.headers["idempotency-key"]);
  });

  it("leaves the key off a read", async () => {
    const api = new FakeApi();
    await open(api).answer("GET", "/v1/workspaces", { idempotent: false });
    expect(api.last()?.headers["idempotency-key"]).toBeUndefined();
  });

  it("asks again while the host is busy, then throws RateLimited past the deadline", async () => {
    const api = new FakeApi();
    api.busy = 50;
    api.busyWaitMs = 20;

    const failure = open(api, 100).answer("POST", "/v1/workspaces", { json: { name: "w" } });
    await expect(failure).rejects.toBeInstanceOf(RateLimited);
    expect(api.asked("POST", "/v1/workspaces").length).toBeGreaterThan(1);
  });

  it("asks again when the call never arrived", async () => {
    let attempts = 0;
    const api = new FakeApi();
    const answer = (input: string, init?: RequestInit) => {
      attempts += 1;
      if (attempts === 1) {
        throw new TypeError("fetch failed");
      }
      return api.handle(input, init);
    };
    const endpoint = new Endpoint(new HttpTransport("https://api.test", api.key, answer as typeof fetch), null);

    const listed = await endpoint.answer("GET", "/v1/workspaces", { idempotent: false });
    expect(attempts).toBe(2);
    expect(listed.workspaces).toHaveLength(3);
  });

  it("refuses an answer of another API major version", async () => {
    const api = new FakeApi();
    api.version = "2";

    await expect(open(api).answer("GET", "/v1", { idempotent: false })).rejects.toMatchObject({
      name: "ProtocolError",
      code: "api_version_unsupported",
    });
  });

  it("throws a refusal as it was, and an answer that is not JSON as ProtocolError", async () => {
    const api = new FakeApi();
    const endpoint = open(api);

    await expect(
      endpoint.answer(
        "GET",
        "/v1/workspaces/11111111-2222-3333-4444-555555555555/resources/notes/Nowhere",
        { idempotent: false },
      ),
    ).rejects.toBeInstanceOf(Refused);

    const text = new HttpTransport("https://api.test", api.key, (async () =>
      new Response("not json", { status: 200 })) as unknown as typeof fetch);
    await expect(
      new Endpoint(text, null).answer("GET", "/v1", { idempotent: false }),
    ).rejects.toBeInstanceOf(ProtocolError);
  });
});

describe("a host in process", () => {
  it("names the addon to install when it is absent", async () => {
    const opening = Tilo.host("https://eu1.tilo.sh", { keyFile: "/home/ada/.local/state/tilo/key" });
    await expect(opening).rejects.toBeInstanceOf(ProtocolError);
    await expect(
      Tilo.host("https://eu1.tilo.sh", { keyFile: "/home/ada/.local/state/tilo/key" }),
    ).rejects.toThrow(NATIVE_PACKAGE);
  });
});
