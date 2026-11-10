/** The generated types are the apps' resources: a class's fields and its actions. */

import { describe, expect, it } from "bun:test";

import { Tilo } from "../src/index.ts";
import { pdfViewer } from "../src/types/index.ts";
import { FakeDoor, PDF } from "./fakeDoor.ts";

function open(house: FakeDoor): Tilo {
  return new Tilo({ key: house.key, door: "https://door.test", fetch: house.fetch });
}

describe("a generated type", () => {
  it("reads the app's fields", async () => {
    const house = new FakeDoor();
    const tilo = open(house);

    const form: pdfViewer.Document = pdfViewer.pdfDocument(tilo.workspace("jane-doe"), PDF);
    await form.load();

    expect(form.page_count).toBe(3);
    expect(form.form_values).toEqual({ surname: "Doe", given_name: "Jane" });
    await tilo.close();
  });

  it("sends an assignment as a field edit", async () => {
    const house = new FakeDoor();
    const tilo = open(house);
    const form = pdfViewer.pdfDocument(tilo.workspace("jane-doe"), PDF);
    await form.load();

    form.reading_fraction = 0.5;
    await form.flush();

    expect(house.edits).toEqual([
      {
        resource: PDF,
        edit: { path: [{ field: "reading_fraction" }], op: "set", value: 0.5 },
      },
    ]);
    await tilo.close();
  });

  it("runs the app's action as its method", async () => {
    const house = new FakeDoor();
    const tilo = open(house);
    const form = pdfViewer.pdfDocument(tilo.workspace("jane-doe"), PDF);
    await form.load();

    // The door answers with the names the form has; the type says what the app declares.
    const answer = (await form.readFormFields()) as unknown as { fields: string[] };

    expect(answer).toEqual({ fields: ["surname", "given_name"] });
    expect(house.asked("tilo_do").at(-1)?.arguments.path).toBe(`${PDF}#read-form-fields`);
    await tilo.close();
  });
});
