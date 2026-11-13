/** The routes, and how a path goes into one. */

import { describe, expect, it } from "bun:test";

import { encodePath, query, routes, splitAction } from "../src/routes.ts";
import { frames } from "../src/sse.ts";

describe("a path into a route", () => {
  it("encodes a resource path segment by segment", () => {
    expect(encodePath("notes/Todo plan")).toBe("notes/Todo%20plan");
    expect(encodePath("notes/Q3%2F4%20plan")).toBe("notes/Q3%2F4%20plan");
    expect(routes.resource("ws-1", "notes/Q3%2F4%20plan")).toBe(
      "/v1/workspaces/ws-1/resources/notes/Q3%2F4%20plan",
    );
  });

  it("keeps a resource named actions one segment", () => {
    expect(routes.resource("ws-1", "notes/actions")).toBe("/v1/workspaces/ws-1/resources/notes/actions");
    expect(routes.action("ws-1", "notes/Todo", "append-paragraph")).toBe(
      "/v1/workspaces/ws-1/resources/notes/Todo/actions/append-paragraph",
    );
  });

  it("encodes a workspace and a run", () => {
    expect(routes.workspace("a b")).toBe("/v1/workspaces/a%20b");
    expect(routes.run("ws-1", "run/1")).toBe("/v1/workspaces/ws-1/runs/run%2F1");
    expect(routes.content("ws-1", "tilo.content:abc")).toBe(
      "/v1/workspaces/ws-1/content/tilo.content%3Aabc",
    );
  });

  it("builds a query, leaving absent fields out", () => {
    expect(query({ path: "notes", limit: 2, cursor: null, kind: undefined })).toBe("?path=notes&limit=2");
    expect(query({})).toBe("");
    expect(query({ path: "a b" })).toBe("?path=a%20b");
  });

  it("splits an action off its resource", () => {
    expect(splitAction("pdf-viewer/application.pdf#read-form-fields")).toEqual({
      resource: "pdf-viewer/application.pdf",
      action: "read-form-fields",
    });
    expect(() => splitAction("notes/Todo")).toThrow(/names no action/);
  });
});

describe("a server-sent event stream", () => {
  async function* chunks(...pieces: string[]): AsyncGenerator<Uint8Array> {
    for (const piece of pieces) {
      yield new TextEncoder().encode(piece);
    }
  }

  it("reads fields, a frame per blank line, the cursor kept", async () => {
    const seen = [];
    for await (const frame of frames(
      await chunks(
        ": keep-alive\n\n",
        'id: 7\nevent: change\ndata: {"path":"notes/Todo"}\n\n',
        'event: presence\ndata: {"here":[]}\n\n',
      ),
    )) {
      seen.push(frame);
    }
    expect(seen).toEqual([
      { id: "7", event: "change", data: '{"path":"notes/Todo"}' },
      { id: "7", event: "presence", data: '{"here":[]}' },
    ]);
  });

  it("joins a frame's data lines and survives a split anywhere", async () => {
    const seen = [];
    for await (const frame of frames(
      await chunks("ev", "ent: run\nda", 'ta: {"phase":\n', 'data: "started"}\n\n'),
    )) {
      seen.push(frame);
    }
    expect(seen).toEqual([{ id: null, event: "run", data: '{"phase":\n"started"}' }]);
  });
});
