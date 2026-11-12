/** Reading a workspace, acting in it, following runs and editing fields. */

import { describe, expect, it } from "bun:test";

import { AskPending, Refused, Run, Tilo } from "../src/index.ts";
import { FakeDoor, PDF, TODO, linkTo } from "./fakeDoor.ts";

function door(): FakeDoor {
  return new FakeDoor();
}

function open(house: FakeDoor): Tilo {
  return new Tilo({ key: house.key, door: "https://door.test", fetch: house.fetch });
}

describe("reading a workspace", () => {
  it("lists apps and a resource's children", async () => {
    const tilo = open(door());
    const workspace = tilo.workspace("jane-doe");

    const apps = await workspace.ls();
    expect(apps.rows.map((row) => row.name)).toEqual(["notes", "pdf-viewer", "terminal"]);
    expect(apps.rows[0]?.state).toBe("running");

    const page = await workspace.ls(TODO);
    expect(page.kind).toBe("notes.Page");
    expect(page.rows.map((row) => row.name)).toEqual(["Buy milk", "Buy bread"]);
    expect((page.actions[0] as { id: string }).id).toBe("append-paragraph");
    await tilo.close();
  });

  it("pages a listing with its cursor", async () => {
    const tilo = open(door());

    const first = await tilo.workspace("jane-doe").ls(TODO, { limit: 1 });
    expect(first.rows.map((row) => row.name)).toEqual(["Buy milk"]);
    expect(first.nextCursor).toBe("1");

    const rest = await first.more();
    expect(rest?.rows.map((row) => row.name)).toEqual(["Buy bread"]);
    expect(rest?.nextCursor).toBeNull();
    expect(await rest?.more()).toBeNull();
    await tilo.close();
  });

  it("is iterated page by page", async () => {
    const tilo = open(door());

    const first = await tilo.workspace("jane-doe").ls(TODO, { limit: 1 });
    const names: string[] = [];
    for await (const row of first) {
      names.push(row.name);
    }

    expect(names).toEqual(["Buy milk", "Buy bread"]);
    await tilo.close();
  });

  it("reads fields, children and kind", async () => {
    const tilo = open(door());

    const page = await tilo.workspace("jane-doe").read(TODO);

    expect(page.kind).toBe("notes.Page");
    expect(page.title).toBe("Todo");
    expect(page.pinned).toBe(false);
    expect(page.children).toEqual([]);
    await tilo.close();
  });

  it("reads a conversation from the chat record", async () => {
    const house = door();
    const chain = "derived:messages@derived:default@tab:0000000000000000";
    house.records.set(`so.tilo.chat:${chain}`, { messages: { m1: { channel: "general", text: "hi" } } });
    const workspace = open(house).workspace("jane-doe");

    const read = (await workspace.record("so.tilo.chat", chain, { parts: true, after: "r0" })) as any;

    expect(read.records[0].value.messages.m1.text).toBe("hi");
    expect(house.asked("tilo_read").at(-1)?.arguments).toEqual({
      path: `record:so.tilo.chat:${chain}`,
      parts: true,
      after: "r0",
    });
    const absent = (await workspace.record("so.tilo.chat")) as any;
    expect(absent.records).toEqual([{ record: "", absent: true }]);
  });

  it("answers search hits", async () => {
    const tilo = open(door());

    const hits = await tilo.workspace("jane-doe").find("todo", "notes");

    expect(hits.map((hit) => hit.path)).toEqual([TODO, `${TODO}#append-paragraph`]);
    expect(hits[1]?.action).toBe("append-paragraph");
    await tilo.close();
  });
});

describe("acting", () => {
  it("answers an action's result and what it sent", async () => {
    const house = door();
    const tilo = open(house);

    const result = await tilo.workspace("jane-doe").do(`${TODO}#append-paragraph`, { text: "Buy milk" });

    expect(result).toEqual({ appended: "c01e" });
    expect(house.asked("tilo_do").at(-1)?.arguments).toEqual({
      path: `${TODO}#append-paragraph`,
      args: { text: "Buy milk" },
    });
    await tilo.close();
  });

  it("throws the host's refusal", async () => {
    const tilo = open(door());

    const failure = tilo.workspace("jane-doe").do(`${TODO}#nonsense`);

    await expect(failure).rejects.toThrow(Refused);
    await expect(failure).rejects.toMatchObject({
      code: "invalid_arguments",
      nextSteps: [`tilo_ls ${TODO} lists its actions`],
    });
    await tilo.close();
  });

  it("answers a pending action as a run with a result", async () => {
    const house = door();
    house.appendPending = true;
    const tilo = open(house);

    const run = (await tilo.workspace("jane-doe").do(`${TODO}#append-paragraph`, { text: "Buy milk" })) as Run;

    expect(run).toBeInstanceOf(Run);
    expect(run.invocationId.startsWith("inv-")).toBe(true);
    expect(await run.result()).toEqual({ appended: "c01e" });
    await tilo.close();
  });

  it("cancels and undoes a run", async () => {
    const house = door();
    house.appendPending = true;
    const tilo = open(house);
    const workspace = tilo.workspace("jane-doe");
    const run = (await workspace.do(`${TODO}#append-paragraph`, { text: "Buy milk" })) as Run;

    const stopped = await run.cancel();
    expect((stopped.result as { cancelled: string }).cancelled).toBe(run.invocationId);

    expect(await run.undo()).toEqual({ undone: 1 });
    expect(house.asked("tilo_do").at(-1)?.arguments.path).toBe(`${TODO}#undo`);
    await tilo.close();
  });

  it("follows a run's output to its end", async () => {
    const house = door();
    house.appendPending = true;
    const tilo = open(house);
    const run = (await tilo.workspace("jane-doe").do(`${TODO}#append-paragraph`, {})) as Run;
    house.events = [
      { kind: "run", phase: "started", invocation_id: run.invocationId, path: `${TODO}#append-paragraph`, run: "Fill", run_id: "mcp:1", time_ms: 1 },
      { kind: "change", path: TODO, change: "c1", sentence: "added Buy milk", time_ms: 2, undone: false },
      { kind: "run", phase: "succeeded", invocation_id: run.invocationId, path: `${TODO}#append-paragraph`, run: "Fill", run_id: "mcp:1", time_ms: 3 },
    ];

    const phases: (string | null)[] = [];
    for await (const event of run.output()) {
      phases.push(event.phase);
    }

    expect(phases).toEqual(["started", "succeeded"]);
    await tilo.close();
  });

  it("raises an ask with its link and answers once the person did", async () => {
    const house = door();
    const tilo = open(house);
    const workspace = tilo.workspace("jane-doe");

    let ask: AskPending | null = null;
    try {
      await workspace.do(`${TODO}#ask`);
    } catch (error) {
      ask = error as AskPending;
    }

    expect(ask).toBeInstanceOf(AskPending);
    expect(ask?.url).toBe("https://mcp.tilo.sh/ask/1");
    expect(ask?.invocationId).toBe("ask-1");

    house.askAnswered = true;
    const answer = (await ask?.wait()) as { result: unknown };
    expect(answer.result).toEqual({ allowed: true });
    await tilo.close();
  });

  it("names the run every call inside is a step of", async () => {
    const house = door();
    const tilo = open(house);
    const workspace = tilo.workspace("jane-doe");

    await workspace.run("Fill Jane's application", async (group) => {
      await workspace.do(`${TODO}#append-paragraph`, { text: "Buy milk" });
      await workspace.do(`${PDF}#read-form-fields`);
      expect(group.invocations.length).toBe(2);
    });

    const calls = house.asked("tilo_do").slice(-2);
    expect(calls[0]?.run?.title).toBe("Fill Jane's application");
    expect(calls[1]?.run?.id).toBe(calls[0]?.run?.id);
    await tilo.close();
  });

  it("hands over the link to the record a run changed", async () => {
    const house = door();
    const tilo = open(house);
    const workspace = tilo.workspace("jane-doe");

    const group = await workspace.run("Fill Jane's application", async (group) => {
      await workspace.do(`${TODO}#append-paragraph`, { text: "Buy milk" });
      await workspace.do(`${PDF}#read-form-fields`);
      await workspace.do(`${TODO}#append-paragraph`, { text: "Buy bread" });
      return group;
    });

    expect(group.links).toEqual([linkTo(TODO)]);
    expect(group.link).toBe(linkTo(TODO));
    await tilo.close();
  });

  it("hands over each record's link once for a run across records", async () => {
    const house = door();
    const tilo = open(house);
    const workspace = tilo.workspace("jane-doe");

    const group = await workspace.run("Fill Jane's application", async (group) => {
      await workspace.do(`${TODO}#append-paragraph`, { text: "Buy milk" });
      await workspace.do(`${PDF}#field-edit`, { edit: { path: "form_values.surname", op: "set", value: "Roe" } });
      return group;
    });

    expect(group.links).toEqual([linkTo(TODO), linkTo(PDF)]);
    expect(group.link).toBeNull();
    await tilo.close();
  });

  it("names a finished run's link", async () => {
    const house = door();
    house.appendPending = true;
    const tilo = open(house);

    const run = (await tilo.workspace("jane-doe").do(`${TODO}#append-paragraph`, { text: "Buy milk" })) as Run;
    expect(run.link).toBeNull();
    await run.result();

    expect(run.link).toBe(linkTo(TODO));
    await tilo.close();
  });

  it("answers the link to any path and refuses a bad one", async () => {
    const house = door();
    const tilo = open(house);
    const workspace = tilo.workspace("jane-doe");

    expect(await workspace.link(PDF)).toBe(linkTo(PDF));
    expect(house.asked("tilo_link").at(-1)?.arguments).toEqual({ path: PDF });
    await workspace.link(TODO, { mode: "desktop" });
    expect(house.asked("tilo_link").at(-1)?.arguments).toEqual({ path: TODO, mode: "desktop" });
    await expect(workspace.link("notes/Nothing")).rejects.toBeInstanceOf(Refused);
    await tilo.close();
  });

  it("undoes a run group as one task", async () => {
    const house = door();
    const tilo = open(house);

    await tilo.workspace("jane-doe").run("Fill Jane's application", async (group) => {
      await tilo.workspace("jane-doe").do(`${TODO}#append-paragraph`, {});
      const undone = await group.undo();
      expect(undone.status).toBe("ok");
    });

    expect(house.asked("tilo").at(-1)?.arguments).toEqual({ program: "undo task" });
    await tilo.close();
  });

  it("answers a program's output and steps", async () => {
    const house = door();
    const tilo = open(house);

    const answer = await tilo.workspace("jane-doe").program("notes.notebook.notes");

    expect(answer.status).toBe("ok");
    expect(answer.output).toEqual(['n1 "Q3 plan"', "1 change in notes"]);
    expect(house.asked("tilo").at(-1)?.arguments).toEqual({ program: "notes.notebook.notes" });
    await tilo.close();
  });

  it("answers a program that previews as preview", async () => {
    const tilo = open(door());

    const answer = await tilo.workspace("jane-doe").program("preview me");

    expect(answer.status).toBe("preview");
    expect(answer.changed).toBe(4);
    await tilo.close();
  });

  it("throws a failing program's refusal", async () => {
    const tilo = open(door());

    const failure = tilo.workspace("jane-doe").program("boom");

    await expect(failure).rejects.toMatchObject({
      code: "compile_error",
      nextSteps: ["tilo_ls notes names its resources"],
    });
    await tilo.close();
  });

  it("keeps a watch's cursor and answers its events", async () => {
    const house = door();
    house.events = [
      { kind: "change", path: TODO, change: "c1", sentence: "one", time_ms: 1, undone: false },
      { kind: "presence", here: [{ who: "u1", name: "Jane", kind: "person" }] },
    ];
    house.watchPage = 1;
    const tilo = open(house);
    const stream = tilo.workspace("jane-doe").watch();

    const first = await stream.next();
    const second = await stream.next();

    expect(first.value?.change).toBe("c1");
    expect(second.value?.here[0]?.name).toBe("Jane");
    expect(house.asked("tilo_watch").at(-1)?.arguments.cursor).toBe("1");
    await stream.return(undefined);
    await tilo.close();
  });

  it("answers an upload's content reference", async () => {
    const house = door();
    const tilo = open(house);
    const workspace = tilo.workspace("jane-doe");

    const content = await workspace.files.upload(new TextEncoder().encode("passport bytes"));

    expect(content).toBe("tilo.content:14");
    expect(house.uploads.length).toBe(1);

    const imported = await workspace.do(`${PDF}#import-pdf-content`, { content, name: "passport.pdf" });
    expect(imported).toEqual({ imported: content });
    await tilo.close();
  });

  it("downloads content in pieces", async () => {
    const house = door();
    const tilo = open(house);
    const workspace = tilo.workspace("jane-doe");
    const content = await workspace.files.upload(new TextEncoder().encode("cover letter"));

    const bytes = await workspace.files.download(content);

    expect(new TextDecoder().decode(bytes)).toBe("cover letter");
    expect(house.asked("tilo_get").map((call) => call.arguments.offset)).toEqual([0, 4, 8]);
    await tilo.close();
  });

  it("answers the file an action makes as its reference", async () => {
    const tilo = open(door());
    const workspace = tilo.workspace("jane-doe");

    const letter = await workspace.do("word-processor#export-content", { format: "docx" });

    expect(letter).toBe("tilo.content:15");
    const bytes = await workspace.files.download(letter as string);
    expect(new TextDecoder().decode(bytes)).toBe("PK cover letter");
    await tilo.close();
  });

  it("refuses to download unknown content", async () => {
    const tilo = open(door());
    const workspace = tilo.workspace("jane-doe");

    const refused = await workspace.files.download("tilo.content:nothing").catch((error) => error);

    expect(refused).toBeInstanceOf(Refused);
    expect(refused.code).toBe("not_found");
    await tilo.close();
  });
});
