/** A resource as an object, and the surface a generated type sends with it. */

import { describe, expect, it } from "bun:test";

import { SurfaceChanged, Tilo } from "../src/index.ts";
import { pdfViewer, sections } from "../src/types/index.ts";
import { FakeApi, PDF, SURFACE, TODO, WORKSPACE_ID } from "./fakeApi.ts";

function open(api: FakeApi): Tilo {
  return new Tilo({ url: "https://api.test", key: api.key, fetch: api.fetch });
}

describe("a resource", () => {
  it("reads a field as an attribute, in either spelling", async () => {
    const api = new FakeApi();
    const form = await open(api).workspace("jane-doe").read(PDF);

    expect(form.page_count).toBe(3);
    expect(form.pageCount).toBe(3);
    expect(form.form_values).toEqual({ surname: "Doe", given_name: "Jane" });
  });

  it("sends an assignment as a field edit", async () => {
    const api = new FakeApi();
    const page = await open(api).workspace("jane-doe").read(TODO);

    page.title = "Done";
    await page.flush();

    expect(api.edits).toEqual([
      { resource: TODO, edit: { path: [{ field: "title" }], op: "set", value: "Done" } },
    ]);
    expect(api.last()?.path).toBe(`/v1/workspaces/${WORKSPACE_ID}/resources/${TODO}/actions/field-edit`);
  });

  it("edits a field one level down, and removes a key", async () => {
    const api = new FakeApi();
    const form = await open(api).workspace("jane-doe").read(PDF);

    (form.form_values as Record<string, unknown>).surname = "Okafor";
    await form.flush();
    await form.remove("form_values", "given_name");

    expect(api.edits[0]?.edit).toEqual({
      path: [{ field: "form_values" }, { key: "surname" }],
      op: "set",
      value: "Okafor",
    });
    expect(api.edits[1]?.edit).toEqual({
      path: [{ field: "form_values" }, { key: "given_name" }],
      op: "remove-key",
      value: null,
    });
  });

  it("runs an action by its camelCase name", async () => {
    const api = new FakeApi();
    const form = await open(api).workspace("jane-doe").read(PDF);

    const read = (await (form.readFormFields as () => Promise<{ fields: { name: string }[] }>)()) as {
      fields: { name: string }[];
    };

    expect(read.fields.map((field) => field.name)).toEqual(["surname", "given_name"]);
    expect(api.last()?.path).toBe(
      `/v1/workspaces/${WORKSPACE_ID}/resources/${PDF}/actions/read-form-fields`,
    );
  });

  it("says to read it before an action of it is called", async () => {
    const api = new FakeApi();
    const form = open(api).workspace("jane-doe").resource(PDF);
    expect(() => (form.readFormFields as () => unknown)()).toThrow(/is not read yet/);
  });
});

describe("a generated type", () => {
  it("reads the app's fields", async () => {
    const api = new FakeApi();
    const form: pdfViewer.Document = pdfViewer.pdfDocument(open(api).workspace("jane-doe"), PDF);
    await form.load();

    expect(form.page_count).toBe(3);
    expect(form.form_values).toEqual({ surname: "Doe", given_name: "Jane" });
  });

  it("reaches an action before any read, and answers its result", async () => {
    // A generated type's method is the action: no `load()` first, and the
    // answer is what the action produced, not a run to follow.
    const api = new FakeApi();
    api.surface = pdfViewer.SURFACE;
    const form: pdfViewer.Document = pdfViewer.pdfDocument(open(api).workspace("jane-doe"), PDF);

    const answer = await form.readFormFields();

    expect(answer.fields.map((field) => field.name)).toEqual(["surname", "given_name"]);
    expect(api.last()?.path).toContain("/actions/read-form-fields");
  });

  it("sends its surface with every call it makes", async () => {
    const api = new FakeApi();
    const form = open(api).workspace("jane-doe").resource(PDF, { surface: api.surface });
    await form.load();

    form.reading_fraction = 0.5;
    await form.flush();
    await form.action("read-form-fields");

    const edits = api.calls.filter((call) => call.path.endsWith("/actions/field-edit"));
    expect(edits[0]?.headers["tilo-surface"]).toBe(SURFACE);
    expect(api.last()?.headers["tilo-surface"]).toBe(SURFACE);
  });

  it("throws SurfaceChanged when the workspace publishes another surface", async () => {
    const api = new FakeApi();
    const form = open(api)
      .workspace("jane-doe")
      .resource(PDF, "so.tilo.pdf-viewer@ffffffffffffffff");
    await form.load();

    const failure = form.action("read-form-fields");

    await expect(failure).rejects.toBeInstanceOf(SurfaceChanged);
    const refused = (await failure.catch((error) => error)) as SurfaceChanged;
    expect(refused.code).toBe("surface_changed");
    expect(refused.message).toContain("tilo sdk gen");
    expect(refused.nextSteps[0]).toContain("tilo sdk gen");
  });

  it("sends no surface for a plain resource", async () => {
    const api = new FakeApi();
    const form = await open(api).workspace("jane-doe").read(PDF);
    await form.action("read-form-fields");

    expect(api.last()?.headers["tilo-surface"]).toBeUndefined();
  });
});

describe("a settings section", () => {
  it("sets a person's and a device's setting through its setter, and reads them back", async () => {
    const api = new FakeApi();
    api.surface = sections.SURFACE;
    const person = "settings/appearance";
    const device = `devices/${"ab".repeat(32)}/settings/power`;
    api.fields[person] = { mode: "light" };
    api.fields[device] = { blank_minutes: 5 };
    const workspace = open(api).workspace("jane-doe");

    const appearance = sections.appearance(workspace, person);
    const power = sections.power(workspace, device);
    await appearance.load();
    await power.load();
    appearance.mode = "dark";
    power.blank_minutes = 10;
    await appearance.flush();
    await power.flush();

    const setters = api.calls.filter((call) => call.path.includes("/actions/set-"));
    expect(setters.map((call) => [call.path, JSON.parse(call.body)])).toEqual([
      [`/v1/workspaces/${WORKSPACE_ID}/resources/${person}/actions/set-mode`, { value: "dark" }],
      [`/v1/workspaces/${WORKSPACE_ID}/resources/${device}/actions/set-blank-minutes`, { value: 10 }],
    ]);
    expect(setters.map((call) => call.headers["tilo-surface"])).toEqual([sections.SURFACE, sections.SURFACE]);
    const appearanceAgain = sections.appearance(workspace, person);
    const powerAgain = sections.power(workspace, device);
    await appearanceAgain.load();
    await powerAgain.load();
    expect(appearanceAgain.mode).toBe("dark");
    expect(powerAgain.blank_minutes).toBe(10);
  });
});
