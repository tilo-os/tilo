/**
 * A visa startup's backend, one agent per applicant, end to end.
 *
 * The backend holds one server key (`TILO_API_KEY`, all workspaces, no
 * expiry) at an API server (`TILO_API_URL`). It makes a workspace per
 * applicant, puts an applicant's passport and application form into theirs
 * and mints a key that reaches that workspace alone. The applicant's agent,
 * with only that key, fills the form in PDF Viewer and writes a cover letter
 * in Writer. The backend then invites the startup's ops person, another
 * person, as a reviewer of Jane's workspace: they follow the link, sign in,
 * and with a key of their own read what the agent did, who did it, and take
 * it back.
 *
 *     TILO_API_URL=http://127.0.0.1:8080 TILO_API_KEY=tilo_sk_… bun sdk/typescript/examples/visa_proof.ts
 *
 * The run prints the reviewer's invitation, then the links to the form and
 * the letter the agent changed, and asks for the key the reviewer made after
 * following the invitation; `TILO_INVITEE` instead names an endpoint that
 * plays the reviewer (the `api-on-relay` harness): it is posted the link and
 * answers the reviewer's key.
 *
 * `VISA_APPLICANTS` sets how many workspaces it makes (3).
 */

import { deflateSync } from "node:zlib";
import { createInterface } from "node:readline/promises";

import { ProtocolError, Refused, Tilo, type Workspace, type WorkspaceInfo } from "tilo-sdk";

const APPLICANTS = Number(process.env.VISA_APPLICANTS ?? "3");
const RUN_TITLE = "Fill Jane Doe's visa application";
const REVIEWER_EMAIL = "rosa@example.com";
const FORM = "pdf-viewer/application.pdf";

const FIRST = ["Jane", "Amara", "Luis", "Mei", "Omar", "Priya", "Tomas", "Yuki", "Noah", "Zara"];
const LAST = ["Doe", "Okafor", "Garcia", "Chen", "Haddad", "Rao", "Novak", "Sato", "Berg", "Ali"];

const FILLED: Record<string, unknown> = {
  surname: { kind: "text", value: "Doe" },
  given_names: { kind: "text", value: "Jane" },
  passport_number: { kind: "text", value: "X1234567" },
  purpose: { kind: "choice", value: "Business" },
  declaration: { kind: "checked", value: { on: true, export: "Yes" } },
};

const LETTER = [
  "Dear Visa Officer,",
  "I apply for a short-stay business visa to attend meetings with our partners in Berlin.",
  "My passport, the completed application form and my itinerary are enclosed.",
  "Yours faithfully, Jane Doe",
];

function say(step: string, detail = ""): void {
  const at = new Date().toISOString().slice(11, 19);
  console.log(`[${at}] ${step}${detail ? `: ${detail}` : ""}`);
}

function check(condition: unknown, what: string): void {
  if (!condition) {
    throw new Error(what);
  }
  console.log(`    ok  ${what}`);
}

function quantile(values: number[], q: number): number {
  const ordered = [...values].sort((a, b) => a - b);
  return ordered[Math.round((ordered.length - 1) * q)] ?? 0;
}

// -- the applicant's documents ----------------------------------------------

/** A small picture standing in for a passport scan. */
function passportPng(): Uint8Array {
  const width = 64;
  const height = 40;
  const rows = Buffer.alloc(height * (1 + width * 3));
  for (let y = 0; y < height; y += 1) {
    const at = y * (1 + width * 3);
    rows[at] = 0;
    for (let x = 0; x < width; x += 1) {
      rows[at + 1 + x * 3] = 40 + x * 3;
      rows[at + 2 + x * 3] = 60 + y * 4;
      rows[at + 3 + x * 3] = 140;
    }
  }
  const chunk = (kind: string, data: Uint8Array): Buffer => {
    const length = Buffer.alloc(4);
    length.writeUInt32BE(data.length);
    const kindAndData = Buffer.concat([Buffer.from(kind, "latin1"), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(zlibCrc(kindAndData));
    return Buffer.concat([length, kindAndData, crc]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = 2;
  const png = Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(rows)),
    chunk("IEND", new Uint8Array()),
  ]);
  return new Uint8Array(png);
}

/** CRC-32, as a PNG's chunks need it. */
function zlibCrc(data: Buffer): number {
  let crc = 0xffffffff;
  for (const byte of data) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = crc & 1 ? (crc >>> 1) ^ 0xedb88320 : crc >>> 1;
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

/** A visa application with an AcroForm: three text fields, a choice and a checkbox. */
function applicationPdf(): Uint8Array {
  const stream = (dictionary: string, content: string): string =>
    `${dictionary.replace(/>>\s*$/, "").trimEnd()} /Length ${content.length} >>\nstream\n${content}\nendstream`;

  const widget = "/DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R";
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R 6 0 R 7 0 R 8 0 R] " +
      "/DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 11 0 R >> >> >> >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Annots [4 0 R 5 0 R 6 0 R 7 0 R 8 0 R] " +
      "/Contents 12 0 R /Resources << /Font << /Helv 11 0 R >> >> >>",
    `<< /Type /Annot /Subtype /Widget /FT /Tx /T (surname) /V () /Rect [200 760 520 780] ${widget} >>`,
    `<< /Type /Annot /Subtype /Widget /FT /Tx /T (given_names) /V () /Rect [200 720 520 740] ${widget} >>`,
    `<< /Type /Annot /Subtype /Widget /FT /Tx /T (passport_number) /V () /Rect [200 680 520 700] ${widget} >>`,
    "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (purpose) /Opt [(Tourism) (Business) " +
      `(Visiting family)] /V (Tourism) /Rect [200 640 400 660] ${widget} >>`,
    "<< /Type /Annot /Subtype /Widget /FT /Btn /T (declaration) /V /Off /AS /Off /Rect [200 600 216 616] " +
      "/F 4 /P 3 0 R /AP << /N << /Yes 9 0 R /Off 10 0 R >> >> >>",
    stream("<< /Type /XObject /Subtype /Form /BBox [0 0 16 16] >>", "0 g 3 3 10 10 re f"),
    stream("<< /Type /XObject /Subtype /Form /BBox [0 0 16 16] >>", "0 G 0.5 0.5 15 15 re S"),
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    stream(
      "<< >>",
      "BT /Helv 12 Tf 40 800 Td (Application for Schengen Visa) Tj 0 -36 Td (Surname) Tj " +
        "0 -40 Td (Given names) Tj 0 -40 Td (Passport number) Tj 0 -40 Td (Purpose of journey) Tj " +
        "0 -40 Td (I declare the above is true) Tj ET",
    ),
  ];
  const pieces = [Buffer.from("%PDF-1.7\n", "latin1")];
  const offsets: number[] = [];
  let at = pieces[0]?.length ?? 0;
  objects.forEach((body, index) => {
    offsets.push(at);
    const piece = Buffer.from(`${index + 1} 0 obj\n${body}\nendobj\n`, "latin1");
    pieces.push(piece);
    at += piece.length;
  });
  const xref = at;
  const trailer = [`xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`];
  for (const offset of offsets) {
    trailer.push(`${String(offset).padStart(10, "0")} 00000 n \n`);
  }
  trailer.push(`trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`);
  pieces.push(Buffer.from(trailer.join(""), "latin1"));
  return new Uint8Array(Buffer.concat(pieces));
}

// -- reading answers ---------------------------------------------------------

interface FormField {
  name: string;
  kind: string;
  path: unknown;
  value: unknown;
}

function valuesOf(fields: FormField[]): Record<string, unknown> {
  return Object.fromEntries(fields.map((field) => [field.name, field.value]));
}

async function fieldsOf(workspace: Workspace): Promise<FormField[]> {
  const read = (await workspace.do(`${FORM}#read-form-fields`)) as { fields: FormField[] };
  return read.fields;
}

/** How the API refused `call`: an HTTP status, or a refusal's code. */
async function refused(call: Promise<unknown>): Promise<string> {
  try {
    await call;
  } catch (error) {
    if (error instanceof ProtocolError) {
      return String(error.status ?? error.code ?? error.message);
    }
    if (error instanceof Refused) {
      return `${error.code}: ${error.message}`;
    }
    throw error;
  }
  return "allowed";
}

/** The reviewer follows `link` and makes a key of their own: `{key, person, joined}`. */
async function reviewerAccepts(link: string): Promise<Record<string, unknown>> {
  const invitee = process.env.TILO_INVITEE;
  if (!invitee) {
    console.log(`Reviewer: open ${link}, sign in, then create a key at the API server.`);
    const asked = createInterface({ input: process.stdin, output: process.stdout });
    const key = (await asked.question("The reviewer's key: ")).trim();
    asked.close();
    return { key, person: null, joined: { role: "editor" } };
  }
  const answer = await fetch(invitee, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ link }),
  });
  return (await answer.json()) as Record<string, unknown>;
}

// -- the proof ---------------------------------------------------------------

async function makeWorkspaces(backend: Tilo): Promise<WorkspaceInfo[]> {
  say(`1. The backend makes ${APPLICANTS} applicant workspaces with its one server key`);
  const made: WorkspaceInfo[] = [];
  const timings: number[] = [];
  for (let index = 0; index < APPLICANTS; index += 1) {
    const name = `Applicant: ${FIRST[index % 10]} ${LAST[Math.floor(index / 10) % 10]}`;
    const began = performance.now();
    made.push(await backend.workspaces.create(name));
    timings.push(performance.now() - began);
  }
  const listingBegan = performance.now();
  const listing = await backend.workspaces.list();
  const listed = (performance.now() - listingBegan) / 1000;
  const ids = new Set(listing.workspaces.map((workspace) => workspace.id));
  const p50 = quantile(timings, 0.5);
  const p95 = quantile(timings, 0.95);
  say(
    "   timings",
    `create p50 ${p50.toFixed(0)} ms, p95 ${p95.toFixed(0)} ms, max ${Math.max(...timings).toFixed(0)} ms; ` +
      `list of ${ids.size} in ${(listed * 1000).toFixed(0)} ms`,
  );
  check(new Set(made.map((workspace) => workspace.id)).size === APPLICANTS, `${APPLICANTS} distinct workspaces, no cap`);
  check(made.every((workspace) => ids.has(workspace.id)), "every one of them is listed");
  check(p95 < 2000, `making one stays fast (p95 ${p95.toFixed(0)} ms < 2 s)`);
  check(listed < 5, `listing them stays fast (${(listed * 1000).toFixed(0)} ms < 5 s)`);
  return made;
}

async function main(): Promise<number> {
  say("API", process.env.TILO_API_URL ?? "");
  const backend = new Tilo();
  say("API says", `${(await backend.info()).server}, version ${(await backend.info()).apiVersion}`);

  const made = await makeWorkspaces(backend);
  const jane = made[0] as WorkspaceInfo;
  check(jane.name === "Applicant: Jane Doe", `the first is ${JSON.stringify(jane.name)}`);

  say("2. The backend puts Jane's documents into her workspace and mints her agent's key");
  const workspace = backend.workspace(jane.id);
  const passportBytes = passportPng();
  const formBytes = applicationPdf();
  const passport = await workspace.files.upload(passportBytes);
  const application = await workspace.files.upload(formBytes);
  say("   uploaded", `passport.png ${passport}, application.pdf ${application}`);
  check(passport.startsWith("tilo.content:") && application.startsWith("tilo.content:"), "both are content");
  const minted = await workspace.keys.mint("Jane Doe's agent");
  say("   minted", `${JSON.stringify(minted.name)} for ${minted.workspace}`);
  check(minted.workspace === jane.id, "the key names Jane's workspace");
  check(minted.token !== "" && minted.token !== process.env.TILO_API_KEY, "it is a key of its own");

  say("3. Jane's agent, with only the minted key, fills the form and writes the letter");
  const agent = new Tilo({ key: minted.token });
  const agentWorkspace = agent.workspace(jane.id);
  const downloaded = await agentWorkspace.files.download(passport);
  check(
    downloaded.length === passportBytes.length && downloaded.every((byte, index) => byte === passportBytes[index]),
    "the agent reads the passport the backend put there",
  );

  const group = await agentWorkspace.run(RUN_TITLE, async (run) => {
    await agentWorkspace.do("pdf-viewer#open");
    const imported = await agentWorkspace.do("pdf-viewer#import-pdf-content", {
      content: application,
      name: "application.pdf",
    });
    say("   imported", JSON.stringify(imported).slice(0, 200));
    const before = await fieldsOf(agentWorkspace);
    say("   fields", before.map((field) => `${field.name} (${field.kind})`).join(", "));
    check(
      Object.keys(FILLED).every((name) => before.some((field) => field.name === name)),
      "the form has every field the agent fills",
    );
    for (const field of before) {
      if (field.name in FILLED) {
        await agentWorkspace.do(`${FORM}#field-edit`, {
          edit: { path: field.path, op: "set", value: FILLED[field.name] },
        });
      }
    }
    const filled = valuesOf(await fieldsOf(agentWorkspace));
    say("   filled", JSON.stringify(filled));
    check(
      Object.entries(FILLED).every(([name, value]) => JSON.stringify(filled[name]) === JSON.stringify(value)),
      "every field holds what the agent set",
    );

    await agentWorkspace.do("word-processor#open");
    for (const paragraph of LETTER) {
      await agentWorkspace.do("word-processor#append-paragraph", { text: paragraph });
    }
    const exported = await agentWorkspace.do("word-processor#export-content", { format: "docx" });
    check(
      typeof exported === "string" && exported.startsWith("tilo.content:"),
      `Writer exports the letter as content (${String(exported)})`,
    );
    return run;
  });
  check(group.invocations.length > 0, `the run grouped the agent's ${group.invocations.length} calls`);

  const formLink = await agentWorkspace.link(FORM);
  say("   links", group.links.join(", "));
  check(group.links.includes(formLink), `the run hands over the link to the form it filled (${formLink})`);

  const exportRef = (await agentWorkspace.do("word-processor#export-content", { format: "docx" })) as string;
  const letter = await workspace.files.download(exportRef);
  check(letter[0] === 0x50 && letter[1] === 0x4b, `the backend downloads a Word file (${letter.length} bytes)`);
  const body = new TextDecoder("latin1").decode(letter);
  check(body.includes("word/document.xml"), "the Word file holds its document part");

  say("4a. Scope: the minted key reaches Jane's workspace and nothing else");
  const other = made[1] as WorkspaceInfo;
  const elsewhere = await refused(agent.workspace(other.id).ls());
  check(elsewhere !== "allowed", `another applicant's workspace refuses it (${elsewhere})`);
  const uploadElsewhere = await refused(agent.workspace(other.id).files.upload(new Uint8Array([1])));
  check(uploadElsewhere !== "allowed", `it puts nothing into another applicant's workspace (${uploadElsewhere})`);
  const listing = await agent.workspaces.list();
  check(
    listing.workspaces.length === 1 && listing.workspaces[0]?.id === jane.id,
    "the account lists the one workspace its key reaches, and no other",
  );
  const creating = await refused(agent.workspaces.create("Applicant: Mallory"));
  check(creating.startsWith("not_permitted"), `the account refuses it: no new workspace (${creating})`);
  const minting = await refused(agent.keys.mint(jane.id, "escalated"));
  check(minting.startsWith("not_permitted"), `it mints no keys (${minting})`);

  say("4b. The backend invites its ops person, another person, as Jane's reviewer");
  const invitation = await workspace.members.invite({ role: "editor", email: REVIEWER_EMAIL });
  say(
    "   invited",
    `${invitation.role} via ${invitation.link.slice(0, 60)}… sent=${invitation.sent} (${invitation.next})`,
  );
  check(invitation.link.startsWith("http"), "the invitation is a link to hand over");
  check(invitation.email === REVIEWER_EMAIL && !invitation.sent, "it names their address and says no mail went");
  say("   for review", `the form: ${formLink}`);
  const accepted = await reviewerAccepts(invitation.link);
  say("   accepted", JSON.stringify(accepted.joined));
  check(
    (accepted.joined as Record<string, unknown> | undefined)?.role === "editor",
    "the link lands them in Jane's workspace as an editor",
  );
  const reviewerPerson = accepted.person as string | null;
  const members = await workspace.members.list();
  say("   members", members.map((member) => `${member.name ?? member.person.slice(0, 8)} (${member.role})`).join(", "));
  check(
    reviewerPerson === null ||
      members.some((member) => member.person === reviewerPerson && member.role === "editor"),
    "the backend sees the reviewer among Jane's workspace's members",
  );
  check(
    members.every((member) => member.name !== "Jane Doe's agent"),
    "no key of the account's names a person in the members list",
  );
  const reviewer = new Tilo({ key: accepted.key as string });

  say("4c. The reviewer, with their own key, finds Jane's workspace by name and reads the filled form and who filled it");
  const listed = await reviewer.workspaces.list();
  const found = listed.named(jane.name);
  check(found !== undefined && found.id === jane.id, `the reviewer's account lists ${JSON.stringify(jane.name)}`);
  check(found?.role === "editor", `with the role they hold there (${found?.role})`);
  const reviewing = reviewer.workspace(jane.name);
  const seen = await reviewing.read(FORM);
  say("   read", `${seen.kind}: ${Object.keys(seen.fields).sort().join(", ")}`);
  const reviewed = valuesOf(await fieldsOf(reviewing));
  check(
    Object.entries(FILLED).every(([name, value]) => JSON.stringify(reviewed[name]) === JSON.stringify(value)),
    `the reviewer sees the filled values: ${JSON.stringify(reviewed)}`,
  );

  const history = (await reviewing.do(`${FORM}#history`, { last: 50 })) as {
    versions?: Record<string, unknown>[];
  };
  const versions = (history.versions ?? []).filter(
    (version): version is Record<string, unknown> => version !== null && typeof version === "object",
  );
  for (const version of versions.slice(0, 5)) {
    say("   history", JSON.stringify(version).slice(0, 200));
  }
  const actorOf = (version: Record<string, unknown>) =>
    (version.actor ?? {}) as Record<string, unknown>;
  const byAgent = versions.filter((version) => actorOf(version).kind === "agent");
  check(byAgent.length > 0, `${byAgent.length} of ${versions.length} changes are the agent's`);
  const names = new Set(byAgent.map((version) => actorOf(version).name));
  check(names.size === 1 && names.has("Jane Doe's agent"), `History names the agent by its key's name (${[...names]})`);
  const colours = new Set(byAgent.map((version) => actorOf(version).colour));
  check(colours.size === 1 && !colours.has(undefined), `in one colour (${[...colours]})`);
  const people = new Set(byAgent.map((version) => actorOf(version).for));
  check(people.size === 1 && !people.has(undefined), `each on behalf of the one person (${[...people]})`);
  const runs = new Map(byAgent.filter((version) => version.run_id).map((version) => [version.run_id, version.run]));
  say("   runs", JSON.stringify([...runs]));
  check([...runs.values()].every((title) => title === RUN_TITLE), `History names the agent's one run ${JSON.stringify(RUN_TITLE)}`);

  say("4d. The reviewer undoes the agent's run");
  const runId = [...runs.keys()][0];
  await reviewing.do(`${FORM}#undo`, { run: runId });
  const back = valuesOf(await fieldsOf(reviewing));
  say("   after undo", JSON.stringify(back));
  check(
    JSON.stringify(back.surname) === JSON.stringify({ kind: "text", value: "" }),
    "the surname is empty again",
  );
  check(
    JSON.stringify(back.purpose) === JSON.stringify({ kind: "choice", value: "Tourism" }),
    "the purpose is back to what the form said",
  );

  say("4e. No VM ran for any of it");
  const usage = await backend.usage();
  say(
    "   usage",
    `host ${JSON.stringify(usage.host)}, plan ${JSON.stringify(usage.plan)}, ${usage.workspaces.length} workspaces`,
  );
  check(usage.vmSeconds === 0 && usage.running.length === 0, `0 VM seconds across ${usage.workspaces.length} workspaces`);

  say("4f. Revoking ends what it reached");
  await backend.keys.revoke(minted.grant);
  const gone = await refused(agentWorkspace.ls());
  check(gone === "401" || gone === "403", `the agent's next call is refused (${gone})`);

  agent.close();
  reviewer.close();
  backend.close();
  say("All of it held");
  return 0;
}

process.exit(await main());
