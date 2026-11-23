/**
 * An in-process API server that speaks `/v1`, as `docs/developers/api.md`
 * describes it, with the answers `crates/core/tilo-api/src/v1` gives.
 *
 * Hand it to a client as its `fetch`: it records every request, so a test can
 * assert what the SDK sent, and its state says what the host answers.
 */

import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import { Contract } from "./openapiContract.ts";

const contract = new Contract();

export const WORKSPACE_ID = "11111111-2222-3333-4444-555555555555";
export const OTHER_ID = "66666666-7777-8888-9999-aaaaaaaaaaaa";
export const INVITED_ID = "abcdef01-2345-6789-abcd-ef0123456789";
export const TODO = "notes/Todo";
export const PDF = "pdf-viewer/application.pdf";
export const TASKS = "tasks/Tracker";
export const SURFACE = "so.tilo.notes@0123456789abcdef";

/** The link a record's host mints for the record `path` names. */
export function linkTo(path: string): string {
  const record = (path.split("#")[0] ?? "").split("/")[1] ?? "";
  return `https://tilo.sh/app/#/@eu1.tilo.sh/w/${WORKSPACE_ID}/so.tilo.notes/${record}`;
}

/** One request the SDK made, as the fake read it. */
export interface Recorded {
  method: string;
  /** The path, its segments decoded one by one. */
  path: string;
  /** The path as it came, percent-encoded. */
  raw: string;
  query: Record<string, string>;
  headers: Record<string, string>;
  body: string;
}

type Answer = Record<string, any>;

/** Keys the API turns away, with its typed refusal of each. */
export const DENIED: Record<string, Answer> = {
  tilo_sk_revoked: {
    code: "key_revoked",
    message: "this key was revoked on 2026-11-09",
    next_steps: ["ask the person for a new key"],
    revoked_at_ms: 1_794_000_000_000,
  },
  tilo_sk_expired: {
    code: "key_expired",
    message: "this key expired on 2026-11-09",
    next_steps: ["ask the person for a new key, or mint one again"],
    expired_at_ms: 1_794_000_000_000,
  },
  tilo_sk_never: {
    code: "invalid_token",
    message: "this is not a key or token this API server issued",
    next_steps: [],
  },
};

export class FakeApi {
  key = "tilo_sk_test";
  /** What the key reaches: all the person's workspaces, or one alone. */
  scope: "all" | "workspace" = "all";
  /** The header every answer names the API's version in. */
  version = "1";
  workspaces: Answer[] = [
    { id: WORKSPACE_ID, name: "jane-doe", host: "eu1", role: "owner" },
    { id: OTHER_ID, name: "john-doe", host: "eu1", role: "owner" },
    { id: INVITED_ID, name: "applicant-jane-doe", host: "eu1", role: "editor" },
  ];
  fields: Record<string, Answer> = {
    [TODO]: { title: "Todo", pinned: false },
    [PDF]: {
      name: "application.pdf",
      page_count: 3,
      form_values: { surname: "Doe", given_name: "Jane" },
    },
    [TASKS]: { issues: {} },
  };
  kinds: Record<string, string> = {
    [TODO]: "notes.Page",
    [PDF]: "pdf-viewer.Document",
    [TASKS]: "tasks.tracker",
  };
  actions: Record<string, Answer[]> = {
    [TODO]: [
      {
        id: "append-paragraph",
        label: "Add paragraph",
        kind: "undoable",
        params: [{ name: "text", ty: "text", required: true }],
      },
      {
        id: "field-edit",
        label: "Edit a field",
        kind: "undoable",
        params: [{ name: "edit", ty: "object", required: true }],
      },
    ],
  };
  calls: Recorded[] = [];
  edits: { resource: string; edit: Answer }[] = [];
  /** The bulk calls the SDK made: each resource, and the members it sent. */
  groups: { resource: string; actions: Answer[] }[] = [];
  uploads: Uint8Array[] = [];
  contents = new Map<string, Uint8Array>();
  keys: Answer[] = [
    {
      grant: "g-root",
      name: "visa-backend",
      scope: "all",
      kind: "key",
      created_at_ms: 1,
      expires_at_ms: null,
      browser_pages: true,
    },
  ];
  members: Answer[] = [
    { person: "cd".repeat(32), name: "Rosa Ortiz", role: "editor", administrator: false },
  ];
  /** The branches made, by the idempotency key each was asked with. */
  branches = new Map<string, Answer>();
  /** Whether the person answered a workspace's ask to run commands
   * unattended: `null` while they have not, then whether they allowed it. */
  unattendedAllowed: boolean | null = null;
  /** How many calls the server says it is briefly unavailable for before answering them. */
  busy = 0;
  /** How many answers the invocation stays pending for, once it is asked about. */
  pendingRuns = 0;
  /** Whether an action waits for the person, and whether they have decided. */
  ask = false;
  askAnswered = false;
  /** The surface the app publishes: a call naming another is `409 surface_changed`. */
  surface = SURFACE;
  newKeyAsked: Answer | null = null;
  polls = 0;
  pollsUntilApproved = 1;
  pollAnswer: Answer | null = null;
  /** The events the workspace's stream carries, and how many the invocation's carries. */
  events: { event: string; data: Answer }[] = [];
  /** How many events one stream answer carries before it ends; all without it. */
  eventsPerStream: number | null = null;
  /** Every invocation the workspace lists, oldest first, as its host keeps them. */
  invocations: Answer[] = [];
  /** What `GET …/invocations/{invocation}` answers once it is no longer pending. */
  finished: Answer = { result: { appended: "c01e" }, resources: [TODO], link: linkTo(TODO) };

  /** The `fetch` this fake answers: hand it to the client. */
  get fetch(): typeof fetch {
    return ((input: string | URL | Request, init?: RequestInit) =>
      this.handle(String(input), init)) as unknown as typeof fetch;
  }

  asked(method: string, path: string): Recorded[] {
    return this.calls.filter((call) => call.method === method && call.path === path);
  }

  last(): Recorded | undefined {
    return this.calls[this.calls.length - 1];
  }

  async handle(url: string, init?: RequestInit): Promise<Response> {
    const parsed = new URL(url);
    const method = init?.method ?? "GET";
    const headers: Record<string, string> = {};
    for (const [name, value] of Object.entries((init?.headers ?? {}) as Record<string, string>)) {
      headers[name.toLowerCase()] = value;
    }
    const body =
      init?.body instanceof Uint8Array
        ? new TextDecoder().decode(init.body)
        : typeof init?.body === "string"
          ? init.body
          : "";
    const segments = parsed.pathname.split("/").filter((part) => part !== "");
    const call: Recorded = {
      method,
      path: `/${segments.map((part) => decodeURIComponent(part)).join("/")}`,
      raw: parsed.pathname,
      query: Object.fromEntries(parsed.searchParams.entries()),
      headers,
      body,
    };
    this.calls.push(call);
    const response = this.answer(call, parsed.pathname, body, headers);
    // What the SDK sent and what this fake answers have to be what
    // `docs/api/openapi.json` describes.
    await contract.check(call, response);
    return response;
  }

  private answer(
    call: Recorded,
    pathname: string,
    body: string,
    headers: Record<string, string>,
  ): Response {
    const method = call.method;
    if (pathname === "/keys/new") {
      this.newKeyAsked = JSON.parse(body || "{}") as Answer;
      return this.json({ approve_url: "https://api.test/keys/approve/abc", poll: "poll-secret", interval: 0, expires_in: 300 }, 201);
    }
    if (pathname === "/keys/poll") {
      this.polls += 1;
      if (this.polls < this.pollsUntilApproved) {
        return this.json({ status: "pending" }, 202);
      }
      return this.json(
        this.pollAnswer ?? {
          key: "tilo_sk_created",
          grant: "g-created",
          name: "Nightly job",
          scope: "all",
          expires_at_ms: 9_000_000_000_000,
        },
      );
    }
    const denied = DENIED[(headers.authorization ?? "").slice(7)];
    if (denied !== undefined) {
      return this.json({ error: denied }, 401);
    }
    if (headers.authorization !== `Bearer ${this.key}`) {
      return this.json(
        { error: { code: "invalid_token", message: "send a key", next_steps: [] } },
        401,
      );
    }
    if (this.busy > 0) {
      this.busy -= 1;
      return this.json(
        {
          error: {
            code: "unavailable",
            message: "the server is briefly unavailable; nothing ran",
            next_steps: ["wait and retry the same call"],
          },
        },
        503,
      );
    }
    return this.route(
      method,
      pathname
        .split("/")
        .filter((part) => part !== "")
        .map((part) => decodeURIComponent(part)),
      call,
    );
  }

  private route(method: string, segments: string[], call: Recorded): Response {
    const [v1, ...rest] = segments;
    if (v1 === "v1" && rest.length === 0) {
      return this.json({
        api_version: "1",
        server: "tilo-api test",
        key: this.keys[0],
      });
    }
    if (v1 !== "v1") {
      return this.json({ error: { code: "not_found", message: "the API has no such route" } }, 404);
    }
    if (rest[0] === "resources" && rest[1] === "account") {
      if (rest[2] === "usage" && method === "GET") {
        return this.json({
          path: "account/usage",
          fields: {
            host: "eu1.tilo.sh",
            subscription: { plan: "alpha", account: "ada", usage: { workspaces: [] } },
          },
        });
      }
      if (rest.at(-2) === "actions" && method === "POST") {
        return this.account(rest.slice(2, -2).join("/"), rest.at(-1) as string, call);
      }
    }
    if (rest[0] === "workspaces" && rest.length > 2) {
      return this.workspaceRoute(method, rest[1] as string, rest.slice(2), call);
    }
    return this.json({ error: { code: "not_found", message: "the API has no such route" } }, 404);
  }

  private workspaceRoute(method: string, workspace: string, rest: string[], call: Recorded): Response {
    if (workspace !== WORKSPACE_ID && !this.workspaces.some((row) => row.id === workspace)) {
      return this.json({ error: { code: "not_found", message: "no such workspace" } }, 404);
    }
    const [kind, ...tail] = rest;
    switch (kind) {
      case "resources": {
        if (tail.length === 0) {
          return this.listing(call);
        }
        if (tail.join("/") === "account/workspaces/actions/unattended") {
          return this.unattended(String((JSON.parse(call.body || "{}") as Answer).workspace), call);
        }
        const action = tail[tail.length - 2] === "actions" ? (tail[tail.length - 1] ?? "") : null;
        const path = (action === null ? tail : tail.slice(0, -2)).join("/");
        if (action === null) {
          return method === "GET" ? this.read(path) : this.refuse("method_not_allowed", 405);
        }
        return this.runAction(path, action, call);
      }
      case "invocations":
        return this.invocation(tail, method, call);
      case "views": {
        const path = call.query.path ?? "";
        if (!(path in this.fields)) {
          return this.refuse("not_found", 404, `${path} names nothing`);
        }
        const view: Answer = {
          path,
          app: "so.tilo.notes",
          shown: "resource",
          text: 'document\n  static_text "Todo"\n  button "Rename" #{action: notes.rename}',
          tree: [
            { role: "static_text", name: "Todo", rect: [0, 0, 40, 20] },
            { role: "button", name: "Rename", rect: [0, 24, 60, 24], action: "notes.rename" },
          ],
          nodes: 2,
          omitted: 0,
          viewport: [1280, 760],
          window_origin: [0, 40],
          revision: "abc",
        };
        if (call.query.picture === "true") {
          view.picture = "tilo.content:look";
        }
        return this.json(view);
      }
      case "programs": {
        const asked = JSON.parse(call.body || "{}") as Answer;
        if (asked.program === "boom") {
          return this.json({ status: "failed", output: [], changed: 0, steps: [] });
        }
        if (asked.program === "preview me") {
          return this.json({ status: "preview", output: ["4 changes in notes"], changed: 4, steps: [] });
        }
        return this.json({
          status: "ok",
          output: ['n1 "Q3 plan"', "1 change in notes"],
          changed: 1,
          steps: [{ number: 1, apps: ["notes"], changed: 1 }],
        });
      }
      case "contents": {
        const hash = tail.join("/");
        if (method === "PUT") {
          const bytes = new TextEncoder().encode(call.body);
          this.uploads.push(bytes);
          this.contents.set(hash, bytes);
          return this.json(
            { content: `tilo.content:${hash}`, offset: bytes.length, size: bytes.length },
            201,
          );
        }
        const data = this.contents.get(hash);
        if (data === undefined) {
          return this.refuse("not_found", 404, "the workspace has no such content");
        }
        return new Response(data as unknown as BodyInit, { status: 200, headers: { "content-type": "application/octet-stream" } });
      }
      case "events": {
        const from = call.headers["last-event-id"];
        const after = from === undefined ? -1 : Number(from);
        const frames = this.events
          .map((event, index) => ({ ...event, id: String(index) }))
          .filter((event) => Number(event.id) > after)
          .slice(0, this.eventsPerStream ?? undefined)
          .map((event) => `id: ${event.id}\nevent: ${event.event}\ndata: ${JSON.stringify(event.data)}\n\n`)
          .join("");
        return new Response(frames, {
          status: 200,
          headers: { "content-type": "text/event-stream" },
        });
      }
      default:
        return this.refuse("not_found", 404, "the API has no such route");
    }
  }

  private listing(call: Recorded): Response {
    const path = call.query.path ?? null;
    if (path === null) {
      return this.json({
        path: "apps",
        rows: [
          { path: "notes", name: "notes", label: "Notes", kind: "app", state: "running", has_children: true, actions: [] },
          { path: "pdf-viewer", name: "pdf-viewer", label: "PDF Viewer", kind: "app", state: "installed", has_children: true, actions: [] },
          { path: "terminal", name: "terminal", label: "Terminal", kind: "app", state: "service", has_children: true, actions: [] },
        ],
        actions: [],
      });
    }
    if (path === "notes") {
      return this.json({
        path,
        kind: "notes",
        rows: [{ path: TODO, name: "Todo", label: "Todo", kind: "notes.Page", has_children: true, actions: [] }],
        actions: [],
      });
    }
    const rows = [
      { path: `${TODO}/c01e`, name: "Buy milk", id: "notes.block:c01e", key: "notes.page/body/blocks/c01e", label: "Buy milk", kind: "notes.Block", has_children: false, actions: [] },
      { path: `${TODO}/c02f`, name: "Buy bread", id: "notes.block:c02f", key: "notes.page/body/blocks/c02f", label: "Buy bread", kind: "notes.Block", has_children: false, actions: [] },
    ];
    const limit = call.query.limit === undefined ? null : Number(call.query.limit);
    const cursor = call.query.cursor;
    if (limit !== null && cursor === undefined) {
      return this.json({ path, kind: this.kinds[path] ?? "notes.Page", rows: rows.slice(0, 1), actions: this.actions[path] ?? [], next_cursor: "1" });
    }
    if (cursor !== undefined) {
      return this.json({ path, kind: this.kinds[path] ?? "notes.Page", rows: rows.slice(1), actions: this.actions[path] ?? [] });
    }
    return this.json({ path, kind: this.kinds[path] ?? "notes.Page", rows, actions: this.actions[path] ?? [] });
  }

  private read(path: string): Response {
    if (!(path in this.fields)) {
      return this.refuse("not_found", 404, `no resource ${path}`);
    }
    const app = path.split("/")[0] as string;
    return this.json({
      path,
      kind: this.kinds[path] ?? "",
      fields: this.fields[path],
      children: [],
      app,
      surface: "the app's surface",
      surface_version: this.surface,
    });
  }

  private runAction(path: string, action: string, call: Recorded): Response {
    const sent = call.headers["tilo-surface"];
    if (sent !== undefined && sent !== this.surface) {
      return this.json(
        {
          error: {
            code: "surface_changed",
            message: `these types were made for ${sent}; the workspace's app publishes ${this.surface}`,
            next_steps: [`write the types again: tilo sdk gen -w ${WORKSPACE_ID}`],
          },
        },
        409,
      );
    }
    const args = JSON.parse(call.body || "{}") as Answer;
    if (path === "invocations" && action === "list") {
      return this.listInvocations(args);
    }
    const invocation = `inv-${this.calls.length}`;
    const whole = `${path}#${action}`;
    if (whole === "search#find") {
      const hits = [
        { path: TODO, kind: "notes.Page", label: "Todo" },
        { path: `${TODO}#append-paragraph`, kind: "notes.Page", label: "Add paragraph", action: "append-paragraph" },
      ];
      return this.json({ status: "ok", invocation, path: whole, result: { query: args.query ?? "", hits } });
    }
    if (action === "share-link") {
      if (!(path in this.fields)) {
        return this.refuse("not_found", 404, `${path} names nothing`);
      }
      return this.json({ status: "ok", invocation, path: whole, result: { url: linkTo(path) } });
    }
    if (action === "group") {
      const members = Array.isArray(args.actions) ? (args.actions as Answer[]) : [];
      this.groups.push({ resource: path, actions: members });
      return this.json({
        status: "ok",
        invocation,
        path: whole,
        result: { applied: members.length },
        resources: [path],
        link: linkTo(path),
      });
    }
    if (whole === "workspace/members#list") {
      return this.json({ status: "ok", invocation, path: whole, result: this.members });
    }
    if (whole === "workspace/invitations#invite") {
      const result: Answer = {
        workspace: WORKSPACE_ID,
        invite: "ab".repeat(32),
        role: args.role,
        link: `https://tilo.sh/#/w/${WORKSPACE_ID}?invite=AAAA`,
        expires_at_ns: 7,
      };
      return this.json({ status: "ok", invocation, path: whole, result });
    }
    if (path === TODO && !["append-paragraph", "field-edit", "ask"].includes(action)) {
      return this.refuse("unknown_action", 404, `no action ${action} of ${path}`);
    }
    if (action === "field-edit") {
      this.edits.push({ resource: path, edit: args.edit ?? {} });
      this.applyEdit(path, args.edit ?? {});
      return this.json({
        status: "ok",
        invocation,
        path: whole,
        result: { edited: true },
        resources: [path],
        link: linkTo(path),
      });
    }
    if (action === "undo") {
      return this.json({ status: "ok", invocation, path: whole, result: { undone: args.steps ?? 1 } });
    }
    if (whole === `${TODO}#ask`) {
      if (!this.askAnswered) {
        return this.json(
          { status: "pending", invocation: "ask-1", ask: { url: "https://api.test/ask/1" } },
          202,
        );
      }
      return this.json({ status: "ok", invocation: "ask-1", path: whole, result: { allowed: true } });
    }
    if (whole === "word-processor#export-content") {
      const letter = new TextEncoder().encode("PK cover letter");
      const hash = bytesToHex(blake3(letter));
      const content = `tilo.content:${hash}`;
      this.contents.set(hash, letter);
      return this.json({ status: "ok", invocation, path: whole, resources: [content] });
    }
    if (whole === `${PDF}#read-form-fields`) {
      return this.json({
        status: "ok",
        invocation,
        path: whole,
        result: {
          file: "application.pdf",
          fields: Object.keys(this.fields[PDF]?.form_values ?? {}).map((name) => ({
            name,
            kind: "text",
            value: {},
            path: null,
            options: [],
            export: null,
            page: null,
            read_only: false,
          })),
        },
      });
    }
    if (this.pendingRuns > 0) {
      this.noteInvocation(invocation, whole, "running", 0);
      return this.json({ status: "pending", invocation, path: whole }, 202);
    }
    this.noteInvocation(invocation, whole, "ended", 1);
    return this.json({
      status: "ok",
      invocation,
      path: whole,
      result: { appended: "c01e" },
      resources: [path],
      link: linkTo(path),
    });
  }

  /** Keep `invocation` as the workspace's host lists it. */
  private noteInvocation(invocation: string, action: string, status: string, changes: number): void {
    this.invocations.push({
      id: invocation,
      calls: [invocation],
      by: { name: "Visa backend", kind: "agent" },
      status,
      started_ms: this.invocations.length + 1,
      action,
      changes,
      undone: 0,
      undoable: changes > 0,
      recorded: changes > 0,
      remembered: true,
    });
  }

  private listedAs(invocation: string, status: string): void {
    for (const listed of this.invocations) {
      if (listed.id === invocation) {
        listed.status = status;
        if (status === "undone") {
          listed.undone = listed.changes;
          listed.undoable = false;
        }
      }
    }
  }

  /** A page of the invocations, newest first; the cursor is where it ends. */
  private listInvocations(args: Answer): Response {
    const newest = [...this.invocations].reverse();
    const limit = Math.min(Number(args.limit ?? 50), 200);
    const start = Number(args.cursor ?? 0);
    const page = newest.slice(start, start + limit);
    return this.json({
      status: "ok",
      path: "invocations#list",
      result: {
        items: page,
        total: newest.length,
        waiting: newest.filter((invocation) => invocation.status === "waiting").length,
        pending: false,
        ...(start + limit < newest.length ? { next_cursor: String(start + limit) } : {}),
      },
    });
  }

  private invocation(tail: string[], method: string, call: Recorded): Response {
    const invocation = tail[0] ?? "";
    if (tail[1] === "cancel") {
      this.listedAs(invocation, "cancelled");
      return this.json({ status: "cancelled", invocation, path: `${TODO}#append-paragraph` });
    }
    if (tail[1] === "undo") {
      this.listedAs(invocation, "undone");
      return this.json({ status: "ok", invocation, undone: [TODO] });
    }
    if (tail[1] === "output") {
      const frames = [
        `event: invocation\ndata: ${JSON.stringify({ phase: "started", invocation, path: `${TODO}#append-paragraph`, title: "Append", time_ms: 1 })}\n\n`,
        `event: invocation\ndata: ${JSON.stringify({ phase: "succeeded", invocation, path: `${TODO}#append-paragraph`, title: "Append", time_ms: 2 })}\n\n`,
      ].join("");
      return new Response(frames, {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      });
    }
    if (method !== "GET") {
      return this.refuse("method_not_allowed", 405);
    }
    if (invocation.startsWith("unattended-")) {
      const workspace = invocation.slice("unattended-".length);
      if (this.unattendedAllowed === null) {
        return this.json({ status: "pending", invocation, ask: {} }, 202);
      }
      if (!this.unattendedAllowed) {
        return this.refuse("refused", 403, "the workspace refused: the person did not allow it");
      }
      for (const row of this.workspaces) {
        if (row.id === workspace) {
          row.unattended = true;
        }
      }
      return this.json({ status: "ok", invocation, result: { workspace, unattended: true } });
    }
    if (invocation === "ask-1") {
      return this.json({ status: "ok", invocation, path: `${TODO}#ask`, result: { allowed: true } });
    }
    if (this.pendingRuns > 0) {
      this.pendingRuns -= 1;
      return this.json({ status: "pending", invocation, path: `${TODO}#append-paragraph` }, 202);
    }
    return this.json({ status: "ok", invocation, path: `${TODO}#append-paragraph`, ...this.finished });
  }

  private applyEdit(resource: string, edit: Answer): void {
    const steps = (edit.path ?? []) as Answer[];
    const first = steps[0];
    if (first === undefined || typeof first.field !== "string") {
      return;
    }
    const field = first.field as string;
    const fields = (this.fields[resource] ??= {});
    if (steps.length === 1) {
      if (edit.op === "set") {
        fields[field] = edit.value;
      }
      return;
    }
    const entry = fields[field];
    const key = steps[1]?.key;
    if (typeof entry !== "object" || entry === null || typeof key !== "string") {
      return;
    }
    if (edit.op === "remove-key") {
      delete (entry as Answer)[key];
    } else {
      (entry as Answer)[key] = edit.value;
    }
  }

  /** An action of the account (`workspaces`, `create`), answered as every action is. */
  private account(resource: string, action: string, call: Recorded): Response {
    const asked = JSON.parse(call.body || "{}") as Answer;
    const ok = (result: unknown) =>
      this.json({ status: "ok", path: `account/${resource}#${action}`, result });
    switch (`${resource}#${action}`) {
      case "workspaces#list": {
        const labels = (asked.labels ?? {}) as Record<string, string>;
        return ok({
          workspaces: this.workspaces.filter((row) =>
            Object.entries(labels).every(
              ([name, value]) => ((row.labels ?? {}) as Record<string, string>)[name] === value,
            ),
          ),
        });
      }
      case "workspaces#create":
        return ok({ workspace: this.createWorkspace(call) });
      case "workspaces#branch":
        return this.branchWorkspace(String(asked.workspace), call);
      case "workspaces#delete":
        return this.deleteWorkspace(String(asked.workspace));
      case "keys#list":
        return ok({ keys: this.keys });
      case "keys#mint":
        return this.mintKey(call);
      default: {
        const grant = resource.startsWith("keys/") ? resource.slice("keys/".length) : "";
        if (action === "revoke" && grant !== "") {
          return ok({ revoked: grant });
        }
        return this.refuse("not_found", 404, `the account has no ${resource}#${action}`);
      }
    }
  }

  private createWorkspace(call: Recorded): Answer {
    const asked = JSON.parse(call.body || "{}") as Answer;
    const created = {
      id: "99999999-8888-7777-6666-555555555555",
      name: String(asked.name ?? ""),
      host: "eu1",
      role: "owner",
    };
    this.workspaces.push(created);
    return created;
  }

  /** A branch of `source`, one of the person's own; the same branch again for a retried key. */
  private branchWorkspace(source: string, call: Recorded): Response {
    if (this.scope === "workspace") {
      return this.refuse("not_permitted", 403, "this key reaches one workspace and no account");
    }
    if (!this.workspaces.some((row) => row.id === source && row.role === "owner")) {
      return this.refuse("not_found", 404, "no such workspace of yours");
    }
    const key = call.headers["idempotency-key"] ?? "";
    const retried = this.branches.get(key);
    if (retried !== undefined) {
      return this.json({ status: "ok", path: "account/workspaces#branch", result: { workspace: retried } });
    }
    const asked = JSON.parse(call.body || "{}") as Answer;
    const made: Answer = {
      id: `bbbbbbbb-0000-0000-0000-${String(this.branches.size + 1).padStart(12, "0")}`,
      name: String(asked.name ?? ""),
      host: "eu1",
      role: "owner",
      ...(asked.labels === undefined ? {} : { labels: asked.labels }),
    };
    this.workspaces.push(made);
    this.branches.set(key, made);
    return this.json({ status: "ok", path: "account/workspaces#branch", result: { workspace: made } });
  }

  /** Running commands unattended in `workspace`: off at once, on only once the person allows it. */
  private unattended(workspace: string, call: Recorded): Response {
    const on = (JSON.parse(call.body || "{}") as Answer).on === true;
    if (!on) {
      for (const row of this.workspaces) {
        if (row.id === workspace) {
          delete row.unattended;
        }
      }
      return this.json({ status: "ok", result: { workspace, unattended: false } });
    }
    return this.json({ status: "pending", invocation: `unattended-${workspace}`, ask: {} }, 202);
  }

  private deleteWorkspace(workspace: string): Response {
    if (!this.workspaces.some((row) => row.id === workspace && row.role === "owner")) {
      return this.refuse("not_found", 404, "no such workspace of yours");
    }
    this.workspaces = this.workspaces.filter((row) => row.id !== workspace);
    return this.json({
      status: "ok",
      path: "account/workspaces#delete",
      result: { deleted: workspace, revoked_keys: ["g-minted"] },
    });
  }

  private mintKey(call: Recorded): Response {
    const asked = JSON.parse(call.body || "{}") as Answer;
    return this.json({
      status: "ok",
      path: "account/keys#mint",
      result: {
        grant: "g-minted",
        name: asked.name ?? "minted",
        token: "tilo_sk_minted",
        workspace: asked.workspace,
        mcp_url: `https://api.test/w/${String(asked.workspace)}/mcp`,
        expires_at_ms: 9_000_000_000_000,
      },
    });
  }

  private refuse(code: string, status: number, message = "no"): Response {
    return this.json({ error: { code, message, next_steps: [] } }, status);
  }

  private json(body: unknown, status = 200, headers: Record<string, string> = {}): Response {
    return new Response(JSON.stringify(body), {
      status,
      headers: { "content-type": "application/json", "tilo-api-version": this.version, ...headers },
    });
  }
}
