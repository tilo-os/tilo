/**
 * An in-process API server that speaks `/v1`, as `docs/developers/api.md`
 * describes it, with the answers `crates/hosting/tilo-api/src/v1` gives.
 *
 * Hand it to a client as its `fetch`: it records every request, so a test can
 * assert what the SDK sent, and its state says what the host answers.
 */

export const WORKSPACE_ID = "11111111-2222-3333-4444-555555555555";
export const OTHER_ID = "66666666-7777-8888-9999-aaaaaaaaaaaa";
export const INVITED_ID = "abcdef01-2345-6789-abcd-ef0123456789";
export const TODO = "notes/Todo";
export const PDF = "pdf-viewer/application.pdf";
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
  };
  kinds: Record<string, string> = { [TODO]: "notes.Page", [PDF]: "pdf-viewer.Document" };
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
  invitations: Answer[] = [];
  /** How many calls the host refuses as busy before it answers them, and the wait it says. */
  busy = 0;
  busyWaitMs = 5;
  /** How many answers the run stays pending for, once it is asked about. */
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
  /** The events the workspace's stream carries, and how many the run's carries. */
  events: { event: string; data: Answer }[] = [];
  /** How many events one stream answer carries before it ends; all without it. */
  eventsPerStream: number | null = null;
  /** What `GET …/runs/{run}` answers once it is no longer pending. */
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
    if (parsed.pathname === "/keys/new") {
      this.newKeyAsked = JSON.parse(body || "{}") as Answer;
      return this.json({ approve_url: "https://api.test/keys/approve/abc", poll: "poll-secret", interval: 0, expires_in: 300 }, 201);
    }
    if (parsed.pathname === "/keys/poll") {
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
            code: "rate_limited",
            message: "the host takes your changes more slowly; nothing ran",
            next_steps: ["wait and retry the same call"],
            retry_after_ms: this.busyWaitMs,
          },
        },
        429,
      );
    }
    return this.route(method, segments.map((part) => decodeURIComponent(part)), call);
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
    if (rest[0] === "account") {
      return this.json({
        host: "eu1.tilo.sh",
        standing: { plan: "alpha", account: "ada", usage: { workspaces: [] } },
      });
    }
    if (rest[0] === "workspaces") {
      if (rest.length === 1) {
        return method === "POST" ? this.json(this.createWorkspace(call), 201) : this.json({ workspaces: this.workspaces });
      }
      if (rest.length === 2) {
        return this.deleteWorkspace(rest[1] as string);
      }
      return this.workspaceRoute(method, rest[1] as string, rest.slice(2), call);
    }
    if (rest[0] === "keys") {
      if (rest.length === 1) {
        return method === "POST" ? this.mintKey(call) : this.json({ keys: this.keys });
      }
      return this.revokeKey(rest[1] as string);
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
        const action = tail[tail.length - 2] === "actions" ? (tail[tail.length - 1] ?? "") : null;
        const path = (action === null ? tail : tail.slice(0, -2)).join("/");
        if (action === null) {
          return method === "GET" ? this.read(path) : this.refuse("method_not_allowed", 405);
        }
        return this.runAction(path, action, call);
      }
      case "runs":
        return this.run(tail, method, call);
      case "search":
        return this.json({
          query: call.query.q ?? "",
          hits: [
            { path: TODO, kind: "notes.Page", label: "Todo" },
            { path: `${TODO}#append-paragraph`, kind: "notes.Page", label: "Add paragraph", action: "append-paragraph" },
          ],
        });
      case "links": {
        const path = call.query.path ?? "";
        if (!(path in this.fields)) {
          return this.refuse("not_found", 404, `${path} names nothing`);
        }
        return this.json({ path, link: linkTo(path) });
      }
      case "members": {
        if (tail.length === 0) {
          return this.json({ workspace, members: this.members, invitations: this.invitations });
        }
        const invited = JSON.parse(call.body || "{}") as Answer;
        const invitation: Answer = {
          workspace,
          invite: "ab".repeat(32),
          role: invited.role,
          link: "https://tilo.sh/i/AAAAAAAAAAAAAAAA#key",
          expires_at_ns: 7,
        };
        if (invited.email !== undefined) {
          Object.assign(invitation, { email: invited.email, sent: false, next: "send the link yourself" });
        }
        return this.json(invitation, 201);
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
      case "uploads": {
        const bytes = new TextEncoder().encode(call.body);
        this.uploads.push(bytes);
        const content = `tilo.content:${bytes.length}`;
        this.contents.set(content, bytes);
        return this.json({ content, size: bytes.length }, 201);
      }
      case "content": {
        const data = this.contents.get(tail.join("/"));
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
      { path: `${TODO}/c01e`, name: "Buy milk", label: "Buy milk", kind: "notes.Block", has_children: false, actions: [] },
      { path: `${TODO}/c02f`, name: "Buy bread", label: "Buy bread", kind: "notes.Block", has_children: false, actions: [] },
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
    const run = `inv-${this.calls.length}`;
    const whole = `${path}#${action}`;
    if (path === TODO && !["append-paragraph", "field-edit", "ask"].includes(action)) {
      return this.refuse("unknown_action", 404, `no action ${action} of ${path}`);
    }
    if (action === "field-edit") {
      this.edits.push({ resource: path, edit: args.edit ?? {} });
      this.applyEdit(path, args.edit ?? {});
      return this.json({
        status: "ok",
        run,
        path: whole,
        result: { edited: true },
        resources: [path],
        link: linkTo(path),
      });
    }
    if (action === "undo") {
      return this.json({ status: "ok", run, path: whole, result: { undone: args.steps ?? 1 } });
    }
    if (whole === `${TODO}#ask`) {
      if (!this.askAnswered) {
        return this.json(
          { status: "pending", run: "ask-1", ask: { url: "https://api.test/ask/1" } },
          202,
        );
      }
      return this.json({ status: "ok", run: "ask-1", path: whole, result: { allowed: true } });
    }
    if (whole === "word-processor#export-content") {
      const letter = new TextEncoder().encode("PK cover letter");
      const content = `tilo.content:${letter.length}`;
      this.contents.set(content, letter);
      return this.json({ status: "ok", run, path: whole, resources: [content] });
    }
    if (whole === `${PDF}#read-form-fields`) {
      return this.json({
        status: "ok",
        run,
        path: whole,
        result: { fields: Object.keys(this.fields[PDF]?.form_values ?? {}) },
      });
    }
    if (this.pendingRuns > 0) {
      return this.json({ status: "pending", run, path: whole }, 202);
    }
    return this.json({
      status: "ok",
      run,
      path: whole,
      result: { appended: "c01e" },
      resources: [path],
      link: linkTo(path),
    });
  }

  private run(tail: string[], method: string, call: Recorded): Response {
    const run = tail[0] ?? "";
    if (tail[1] === "cancel") {
      return this.json({ status: "cancelled", run, path: `${TODO}#append-paragraph` });
    }
    if (tail[1] === "undo") {
      return this.json({ status: "ok", run, undone: [TODO] });
    }
    if (tail[1] === "output") {
      const frames = [
        `event: run\ndata: ${JSON.stringify({ phase: "started", run, path: `${TODO}#append-paragraph`, title: "Append", time_ms: 1 })}\n\n`,
        `event: run\ndata: ${JSON.stringify({ phase: "succeeded", run, path: `${TODO}#append-paragraph`, title: "Append", time_ms: 2 })}\n\n`,
      ].join("");
      return new Response(frames, {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      });
    }
    if (method !== "GET") {
      return this.refuse("method_not_allowed", 405);
    }
    if (run === "ask-1") {
      return this.json({ status: "ok", run, path: `${TODO}#ask`, result: { allowed: true } });
    }
    if (this.pendingRuns > 0) {
      this.pendingRuns -= 1;
      return this.json({ status: "pending", run, path: `${TODO}#append-paragraph` }, 202);
    }
    return this.json({ status: "ok", run, path: `${TODO}#append-paragraph`, ...this.finished });
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

  private deleteWorkspace(workspace: string): Response {
    if (!this.workspaces.some((row) => row.id === workspace && row.role === "owner")) {
      return this.refuse("not_found", 404, "no such workspace of yours");
    }
    this.workspaces = this.workspaces.filter((row) => row.id !== workspace);
    return this.json({ deleted: workspace, revoked_keys: ["g-minted"] });
  }

  private mintKey(call: Recorded): Response {
    const asked = JSON.parse(call.body || "{}") as Answer;
    return this.json(
      {
        grant: "g-minted",
        name: asked.name ?? "minted",
        token: "tilo_sk_minted",
        workspace: asked.workspace,
        mcp_url: `https://api.test/w/${String(asked.workspace)}/mcp`,
        expires_at_ms: 9_000_000_000_000,
      },
      201,
    );
  }

  private revokeKey(grant: string): Response {
    return this.json({ revoked: grant });
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
