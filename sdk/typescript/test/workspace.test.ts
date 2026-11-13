/** Reading a workspace, acting in it, following runs, editing fields and watching. */

import { describe, expect, it } from "bun:test";

import { AskPending, Refused, Run, Tilo } from "../src/index.ts";
import { FakeApi, PDF, TODO, WORKSPACE_ID, linkTo } from "./fakeApi.ts";

function open(api: FakeApi): Tilo {
  return new Tilo({ url: "https://api.test", key: api.key, fetch: api.fetch });
}

describe("reading a workspace", () => {
  it("lists apps and a resource's children", async () => {
    const api = new FakeApi();
    const workspace = open(api).workspace("jane-doe");

    const apps = await workspace.ls();
    expect(apps.path).toBe("apps");
    expect(apps.rows.map((row) => row.name)).toEqual(["notes", "pdf-viewer", "terminal"]);
    expect(apps.rows[0]?.state).toBe("running");

    const page = await workspace.ls(TODO);
    expect(page.kind).toBe("notes.Page");
    expect(page.rows.map((row) => row.name)).toEqual(["Buy milk", "Buy bread"]);
    expect(page.actions[0]?.id).toBe("append-paragraph");
    expect(page.actions[0]?.params[0]?.name).toBe("text");
    expect(api.last()?.path).toBe(`/v1/workspaces/${WORKSPACE_ID}/resources`);
    expect(api.last()?.query).toEqual({ path: TODO });
  });

  it("pages a listing with its cursor", async () => {
    const api = new FakeApi();
    const first = await open(api).workspace("jane-doe").ls(TODO, { limit: 1 });

    expect(first.rows.map((row) => row.name)).toEqual(["Buy milk"]);
    expect(first.nextCursor).toBe("1");
    expect(api.last()?.query).toEqual({ path: TODO, limit: "1" });

    const rest = await first.more();
    expect(rest?.rows.map((row) => row.name)).toEqual(["Buy bread"]);
    expect(rest?.nextCursor).toBeNull();
    expect(await rest?.more()).toBeNull();
  });

  it("is iterated page by page", async () => {
    const api = new FakeApi();
    const names: string[] = [];
    for await (const row of await open(api).workspace("jane-doe").ls(TODO, { limit: 1 })) {
      names.push(row.name);
    }
    expect(names).toEqual(["Buy milk", "Buy bread"]);
  });

  it("reads fields, children and the app's surface", async () => {
    const api = new FakeApi();
    const page = await open(api).workspace("jane-doe").read(TODO);

    expect(page.kind).toBe("notes.Page");
    expect(page.title).toBe("Todo");
    expect(page.pinned).toBe(false);
    expect(page.children).toEqual([]);
    expect(page.raw.surface_version).toBe(api.surface);
  });

  it("answers search hits", async () => {
    const api = new FakeApi();
    const hits = await open(api).workspace("jane-doe").find("todo", "notes", 5);

    expect(hits.map((hit) => hit.path)).toEqual([TODO, `${TODO}#append-paragraph`]);
    expect(hits[1]?.action).toBe("append-paragraph");
    expect(api.last()?.query).toEqual({ q: "todo", app: "notes", limit: "5" });
  });

  it("answers a record's link", async () => {
    const api = new FakeApi();
    const link = await open(api).workspace("jane-doe").link(TODO, { mode: "app" });

    expect(link).toBe(linkTo(TODO));
    expect(api.last()?.query).toEqual({ path: TODO, mode: "app" });
  });

  it("lists the people in it, and invites one more", async () => {
    const api = new FakeApi();
    const workspace = open(api).workspace("jane-doe");

    const members = await workspace.members.list();
    expect(members.map((member) => member.name)).toEqual(["Rosa Ortiz"]);
    expect(members[0]?.role).toBe("editor");
    expect(api.last()?.path).toBe(`/v1/workspaces/${WORKSPACE_ID}/members`);

    const invitation = await workspace.members.invite({ role: "editor", email: "rosa@example.com" });
    expect(invitation.link.startsWith("https://")).toBe(true);
    expect(invitation.email).toBe("rosa@example.com");
    expect(invitation.sent).toBe(false);
    expect(api.last()?.body).toBe('{"role":"editor","email":"rosa@example.com"}');
  });
});

describe("acting", () => {
  it("answers an action's result, at its route, with its arguments as the body", async () => {
    const api = new FakeApi();
    const result = await open(api).workspace("jane-doe").do(`${TODO}#append-paragraph`, { text: "Buy milk" });

    expect(result).toEqual({ appended: "c01e" });
    expect(api.last()?.method).toBe("POST");
    expect(api.last()?.path).toBe(`/v1/workspaces/${WORKSPACE_ID}/resources/${TODO}/actions/append-paragraph`);
    expect(api.last()?.body).toBe('{"text":"Buy milk"}');
  });

  it("answers a file an action made as its content reference", async () => {
    const api = new FakeApi();
    const letter = await open(api).workspace("jane-doe").do("word-processor#export-content", { format: "docx" });
    expect(letter).toBe("tilo.content:15");
  });

  it("throws the host's refusal", async () => {
    const api = new FakeApi();
    const failure = open(api)
      .workspace("jane-doe")
      .do(`${TODO}#nonsense`);

    await expect(failure).rejects.toMatchObject({ code: "unknown_action" });
  });

  it("runs a tilolang program as one step", async () => {
    const api = new FakeApi();
    const answer = await open(api).workspace("jane-doe").program("notes.notebook.notes.filter(.pinned)");

    expect(answer.status).toBe("ok");
    expect(answer.changed).toBe(1);
    expect(answer.output).toEqual(['n1 "Q3 plan"', "1 change in notes"]);
    expect(api.last()?.path).toBe(`/v1/workspaces/${WORKSPACE_ID}/programs`);
  });
});

describe("a run", () => {
  it("is what an unfinished action answers, and follows to its result", async () => {
    const api = new FakeApi();
    api.pendingRuns = 2;
    const tilo = open(api);

    const run = (await tilo.workspace("jane-doe").do(`${TODO}#append-paragraph`, { text: "Buy milk" })) as Run;
    expect(run).toBeInstanceOf(Run);
    expect(run.invocationId).toBe("inv-2");

    expect(await run.result()).toEqual({ appended: "c01e" });
    expect(run.link).toBe(linkTo(TODO));
    expect(run.path).toBe(`${TODO}#append-paragraph`);
    expect(api.last()?.query).toEqual({ wait: "60" });
  });

  it("is stopped, and undone, at its own routes", async () => {
    const api = new FakeApi();
    api.pendingRuns = 1;
    const tilo = open(api);

    const run = (await tilo.workspace("jane-doe").do(`${TODO}#append-paragraph`, {})) as Run;
    const cancelled = await run.cancel();
    expect(cancelled.status).toBe("cancelled");
    expect(api.last()?.path).toBe(`/v1/workspaces/${WORKSPACE_ID}/runs/${run.invocationId}/cancel`);

    expect((await run.undo()).undone).toEqual([TODO]);
    expect(api.last()?.path).toBe(`/v1/workspaces/${WORKSPACE_ID}/runs/${run.invocationId}/undo`);
  });

  it("answers its events as a stream, ending when it ends", async () => {
    const api = new FakeApi();
    api.pendingRuns = 1;
    const run = (await open(api).workspace("jane-doe").do(`${TODO}#append-paragraph`, {})) as Run;

    const phases: (string | null)[] = [];
    for await (const event of run.output()) {
      phases.push(event.phase);
    }

    expect(phases).toEqual(["started", "succeeded"]);
    expect(api.last()?.headers.accept).toBe("text/event-stream, application/octet-stream");
  });

  it("waits for the person, and answers what they allowed", async () => {
    const api = new FakeApi();
    const tilo = open(api);

    const failure = tilo.workspace("jane-doe").do(`${TODO}#ask`, {});

    await expect(failure).rejects.toBeInstanceOf(AskPending);
    const ask = (await failure.catch((error) => error)) as AskPending;
    expect(ask.url).toBe("https://api.test/ask/1");
    expect(ask.invocationId).toBe("ask-1");

    api.askAnswered = true;
    expect(await ask.wait()).toEqual({ allowed: true });
  });
});

describe("a grouped run", () => {
  it("names its run and title on its calls, and keeps the links they changed", async () => {
    const api = new FakeApi();
    const tilo = open(api);
    const workspace = tilo.workspace("jane-doe");

    const links = await workspace.run("Fill Jane's application", async (run) => {
      await workspace.do(`${TODO}#append-paragraph`, { text: "Buy milk" });
      await workspace.do(`${PDF}#read-form-fields`, {});
      return run;
    });

    const calls = api.asked("POST", `/v1/workspaces/${WORKSPACE_ID}/resources/${TODO}/actions/append-paragraph`);
    expect(calls[0]?.headers["tilo-run"]).toBe(links.id);
    expect(calls[0]?.headers["tilo-run-title"]).toBe("Fill Jane's application");
    expect(links.invocations).toHaveLength(2);
    expect(links.link).toBe(linkTo(TODO));
    expect(links.links).toContain(linkTo(TODO));
  });

  it("names the title on every call, so every step's row reads it", async () => {
    const api = new FakeApi();
    const workspace = open(api).workspace("jane-doe");

    await workspace.run("Two steps", async () => {
      await workspace.do(`${TODO}#append-paragraph`, {});
      await workspace.do(`${TODO}#append-paragraph`, {});
    });

    const calls = api.asked("POST", `/v1/workspaces/${WORKSPACE_ID}/resources/${TODO}/actions/append-paragraph`);
    expect(calls[0]?.headers["tilo-run-title"]).toBe("Two steps");
    expect(calls[1]?.headers["tilo-run-title"]).toBe("Two steps");
    expect(calls[1]?.headers["tilo-run"]).toBe(calls[0]?.headers["tilo-run"]);
  });
});

describe("a workspace's stream", () => {
  it("yields changes, run events and presence, as they come", async () => {
    const api = new FakeApi();
    api.events = [
      { event: "change", data: { path: TODO, change: "edit", sentence: "Jane added a paragraph", by: "ada", run: "inv-1", time_ms: 5, undone: false } },
      { event: "presence", data: { here: [{ who: "ada", name: "Ada", kind: "person" }] } },
    ];
    const workspace = open(api).workspace("jane-doe");

    const seen = [];
    for await (const event of workspace.watch(TODO)) {
      seen.push(event);
      if (seen.length === 2) {
        break;
      }
    }

    expect(seen[0]?.kind).toBe("change");
    expect(seen[0]?.sentence).toBe("Jane added a paragraph");
    expect(seen[0]?.cursor).toBe("0");
    expect(seen[1]?.kind).toBe("presence");
    expect(seen[1]?.here).toEqual([{ who: "ada", name: "Ada", kind: "person" }]);
    expect(api.last()?.query).toEqual({ path: TODO });
  });

  it("reads on from its cursor when the stream ends", async () => {
    const api = new FakeApi();
    api.events = [
      { event: "change", data: { path: TODO, sentence: "one" } },
      { event: "change", data: { path: TODO, sentence: "two" } },
      { event: "change", data: { path: TODO, sentence: "three" } },
    ];
    api.eventsPerStream = 1;
    const workspace = open(api).workspace("jane-doe");

    const cursors: (string | null)[] = [];
    for await (const event of workspace.watch()) {
      cursors.push(event.cursor);
      if (cursors.length === 3) {
        break;
      }
    }

    expect(cursors).toEqual(["0", "1", "2"]);
    const opened = api.asked("GET", `/v1/workspaces/${WORKSPACE_ID}/events`);
    expect(opened[0]?.headers["last-event-id"]).toBeUndefined();
    expect(opened[1]?.headers["last-event-id"]).toBe("0");
    expect(opened[2]?.headers["last-event-id"]).toBe("1");
  });
});

describe("files", () => {
  it("uploads bytes and answers their content reference", async () => {
    const api = new FakeApi();
    const content = await open(api)
      .workspace("jane-doe")
      .files.upload(new TextEncoder().encode("passport, in bytes"));

    expect(content).toBe("tilo.content:18");
    expect(api.last()?.method).toBe("POST");
    expect(api.last()?.path).toBe(`/v1/workspaces/${WORKSPACE_ID}/uploads`);
    expect(api.last()?.headers["content-type"]).toBe("application/octet-stream");
  });

  it("downloads the bytes an action made", async () => {
    const api = new FakeApi();
    api.contents.set("tilo.content:2", new Uint8Array([0x50, 0x4b]));
    const bytes = await open(api).workspace("jane-doe").files.download("tilo.content:2");

    expect(Array.from(bytes)).toEqual([0x50, 0x4b]);
    expect(api.last()?.headers.accept).toBe("text/event-stream, application/octet-stream");
  });
});
