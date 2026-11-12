/** A resource as an object: fields, actions and field edits. */

import { describe, expect, it } from "bun:test";

import { kebab, segments, Tilo } from "../src/index.ts";
import { FakeDoor, PDF, TODO } from "./fakeDoor.ts";

function open(house: FakeDoor): Tilo {
  return new Tilo({ key: house.key, door: "https://door.test", fetch: house.fetch });
}

describe("a resource", () => {
  it("reads a field from the snapshot", async () => {
    const tilo = open(new FakeDoor());

    const page = await tilo.workspace("jane-doe").read(TODO);

    expect(page.title).toBe("Todo");
    expect(page.fields.title).toBe("Todo");
    await tilo.close();
  });

  it("names a field in either spelling", async () => {
    const tilo = open(new FakeDoor());
    const document = await tilo.workspace("jane-doe").read(PDF);

    expect(document.formValues).toEqual(document.form_values);
    await tilo.close();
  });

  it("sends an assignment as a field edit", async () => {
    const house = new FakeDoor();
    const tilo = open(house);
    const page = tilo.workspace("jane-doe").resource(TODO);
    await page.load();

    page.title = "Done";
    await page.flush();

    expect(house.edits).toEqual([
      { resource: TODO, edit: { path: [{ field: "title" }], op: "set", value: "Done" } },
    ]);
    expect(house.fields[TODO]?.title).toBe("Done");
    await tilo.close();
  });

  it("edits a map one key down and takes an entry away", async () => {
    const house = new FakeDoor();
    const tilo = open(house);
    const document = await tilo.workspace("jane-doe").read(PDF);

    const form = document.formValues as Record<string, unknown>;
    form.surname = "Roe";
    await document.flush();

    expect(house.edits.at(-1)).toEqual({
      resource: PDF,
      edit: { path: [{ field: "form_values" }, { key: "surname" }], op: "set", value: "Roe" },
    });
    expect(house.fields[PDF]?.form_values.surname).toBe("Roe");

    await document.remove("form_values", "given_name");
    expect(house.edits.at(-1)?.edit.op).toBe("remove-key");
    expect(house.fields[PDF]?.form_values).toEqual({ surname: "Roe" });
    await tilo.close();
  });

  it("names an item as a step", async () => {
    const house = new FakeDoor();
    const tilo = open(house);
    const page = await tilo.workspace("jane-doe").read(TODO);

    await page.edit(["blocks", { item: "c01e" }, "text"], "Buy oat milk");

    expect(house.edits.at(-1)?.edit.path).toEqual([
      { field: "blocks" },
      { item: "c01e" },
      { key: "text" },
    ]);
    await tilo.close();
  });

  it("runs a method as the action", async () => {
    const house = new FakeDoor();
    const tilo = open(house);
    const document = await tilo.workspace("jane-doe").read(PDF);

    const readFormFields = document.readFormFields as () => Promise<unknown>;
    const result = await readFormFields();

    expect(result).toEqual({ fields: ["surname", "given_name"] });
    expect(house.asked("tilo_do").at(-1)?.arguments.path).toBe(`${PDF}#read-form-fields`);
    await tilo.close();
  });

  it("asks for a read before its fields", async () => {
    const tilo = open(new FakeDoor());
    const resource = tilo.workspace("jane-doe").resource(TODO);

    expect(() => resource.title).toThrow(/load\(\)/);
    await tilo.close();
  });
});

describe("names", () => {
  it("maps snake and camel case to kebab", () => {
    expect(kebab("read_form_fields")).toBe("read-form-fields");
    expect(kebab("readFormFields")).toBe("read-form-fields");
    expect(kebab("read-form-fields")).toBe("read-form-fields");
    expect(kebab("open")).toBe("open");
    expect(kebab("addRequest")).toBe("add-request");
  });

  it("builds a field path", () => {
    expect(segments(["form_values", "surname"])).toEqual([
      { field: "form_values" },
      { key: "surname" },
    ]);
    expect(segments([{ item: "c01e" }])).toEqual([{ item: "c01e" }]);
    expect(() => segments([])).toThrow();
  });
});
