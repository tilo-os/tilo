/**
 * An in-process door that speaks the SDK's wire.
 *
 * It takes MCP over POST with sessions, typed `_meta`, the account resource,
 * content upload and the key endpoints, and it records what was asked so a
 * test can assert what the SDK sent.
 */

export const SURFACE = "so.tilo.mcp";
export const TODO = "notes/Todo";
export const PDF = "pdf-viewer/application.pdf";
export const WORKSPACE_ID = "11111111-2222-3333-4444-555555555555";
export const OTHER_ID = "66666666-7777-8888-9999-aaaaaaaaaaaa";

export interface RecordedCall {
  tool: string;
  arguments: Record<string, any>;
  run: Record<string, unknown> | null;
  structured: boolean | undefined;
  workspace: string | null;
}

type Answer = Record<string, any>;

export const INVITED_ID = "abcdef01-2345-6789-abcd-ef0123456789";

export class FakeDoor {
  key = "tilo_sk_test";
  scope: "account" | "workspace" = "account";
  workspaces: Answer[] = [
    { id: WORKSPACE_ID, name: "jane-doe", host: "eu1", role: "owner" },
    { id: OTHER_ID, name: "john-doe", host: "eu1", role: "owner" },
    { id: INVITED_ID, name: "applicant-jane-doe", host: "eu1", role: "editor" },
  ];
  hosts: Answer[] = [{ name: "eu1", default: true }];
  fields: Record<string, Record<string, any>> = {
    [TODO]: { title: "Todo", pinned: false },
    [PDF]: {
      name: "application.pdf",
      page_count: 3,
      form_values: { surname: "Doe", given_name: "Jane" },
    },
  };
  kinds: Record<string, string> = { [TODO]: "notes.Page", [PDF]: "pdf-viewer.Document" };
  sessions = new Set<string>();
  opened = 0;
  calls: RecordedCall[] = [];
  edits: { resource: string; edit: Answer }[] = [];
  uploads: Uint8Array[] = [];
  contents = new Map<string, Uint8Array>();
  /** The most bytes one tilo_get answers, small so a test sees paging. */
  piece = 4;
  uploadSessions: (string | null)[] = [];
  keys: Answer[] = [
    {
      grant: "g-root",
      name: "visa-backend",
      scope: "account",
      kind: "key",
      created_at_ms: 1,
      expires_at_ms: 2,
    },
  ];
  asks: Record<string, Answer> = {};
  polls = 0;
  stream = false;
  watchPage: number | null = null;
  appendPending = false;
  askAnswered = false;
  pollsUntilDone = 1;
  events: Answer[] = [];
  finished: Record<string, unknown> = {};
  newKeyAsked: Answer | null = null;
  polledKey: Answer | null = null;

  /** The `fetch` this fake answers: hand it to the client. */
  get fetch(): typeof fetch {
    return ((input: string | URL | Request, init?: RequestInit) =>
      this.handle(String(input), init)) as unknown as typeof fetch;
  }

  asked(tool: string): RecordedCall[] {
    return this.calls.filter((call) => call.tool === tool);
  }

  async handle(url: string, init?: RequestInit): Promise<Response> {
    const parsed = new URL(url);
    const path = parsed.pathname;
    const method = init?.method ?? "GET";
    const headers = (init?.headers ?? {}) as Record<string, string>;
    if (method === "PUT" && path.endsWith("/content")) {
      return this.upload(init, headers);
    }
    if (path === "/keys/new") {
      this.newKeyAsked = JSON.parse(String(init?.body ?? "{}")) as Answer;
      return json({
        approve_url: "https://mcp.tilo.sh/keys/approve/abc",
        poll: "poll-secret",
        interval: 0,
        expires_in: 300,
      });
    }
    if (path === "/keys/poll") {
      this.polls += 1;
      return this.polledKey === null ? new Response("{}", { status: 202 }) : json(this.polledKey);
    }
    return this.mcp(path, method, headers, init);
  }

  private upload(init?: RequestInit, headers: Record<string, string> = {}): Response {
    if (!headers.authorization) {
      return json({ error: { code: "unauthorized", message: "send a key", next_steps: [] } }, 401);
    }
    const body = init?.body;
    const bytes =
      body instanceof Uint8Array ? body : new TextEncoder().encode(String(body ?? ""));
    this.uploads.push(bytes);
    this.contents.set(`tilo.content:${bytes.length}`, bytes);
    this.uploadSessions.push(headers["mcp-session-id"] ?? null);
    return json({ content: `tilo.content:${bytes.length}`, size: bytes.length }, 201);
  }

  private mcp(
    path: string,
    method: string,
    headers: Record<string, string>,
    init?: RequestInit,
  ): Response {
    let workspace: string | null = null;
    if (path.startsWith("/w/")) {
      workspace = path.split("/")[2] ?? null;
      if (!path.endsWith("/mcp")) {
        return new Response("no such endpoint", { status: 404 });
      }
    } else if (path !== "/mcp" && path !== "/mcp/") {
      return new Response("no such endpoint", { status: 404 });
    }
    const authorization = headers.authorization ?? "";
    if (!authorization.startsWith("Bearer ") || authorization.slice(7) !== this.key) {
      return new Response("sign in to use Tilo", { status: 401 });
    }
    if (workspace !== null && this.scope === "workspace" && workspace !== WORKSPACE_ID) {
      return new Response("this grant does not reach that workspace", { status: 403 });
    }
    if (method === "DELETE") {
      this.sessions.delete(headers["mcp-session-id"] ?? "");
      return new Response("", { status: 204 });
    }
    const body = JSON.parse(String(init?.body ?? "{}")) as Answer;
    if (body.method === "initialize") {
      this.opened += 1;
      const session = `session-${this.opened}`;
      this.sessions.add(session);
      return json(
        {
          jsonrpc: "2.0",
          id: body.id,
          result: {
            protocolVersion: "2025-06-18",
            capabilities: { tools: { listChanged: true } },
            serverInfo: { name: "tilo", version: "0" },
          },
        },
        200,
        { "mcp-session-id": session },
      );
    }
    if (body.id === undefined || body.id === null) {
      return new Response("", { status: 202 });
    }
    if (!this.sessions.has(headers["mcp-session-id"] ?? "")) {
      return new Response("unknown session; initialize again", { status: 404 });
    }
    if (body.method !== "tools/call") {
      return json({ jsonrpc: "2.0", id: body.id, error: { code: -32601, message: "Method not found" } });
    }
    const params = (body.params ?? {}) as Answer;
    const surface = ((params._meta ?? {})[SURFACE] ?? {}) as Answer;
    const name = String(params.name ?? "");
    const args = (params.arguments ?? {}) as Answer;
    this.calls.push({
      tool: name,
      arguments: args,
      run: (surface.run ?? null) as Record<string, unknown> | null,
      structured: surface.structured as boolean | undefined,
      workspace,
    });
    const answer = this.tool(name, args, workspace);
    const messages: Answer[] = [];
    const result: Answer =
      answer === null
        ? { content: [{ type: "text", text: "not a typed answer" }], isError: true }
        : {
            content: [{ type: "text", text: JSON.stringify(answer) }],
            structuredContent: answer,
            isError: answer.status === "failed",
          };
    if (this.stream) {
      messages.push({ jsonrpc: "2.0", method: "notifications/tools/list_changed" });
    }
    messages.push({ jsonrpc: "2.0", id: body.id, result });
    if (this.stream) {
      const text = messages
        .map((message) => `event: message\ndata: ${JSON.stringify(message)}\n\n`)
        .join("");
      return new Response(text, { status: 200, headers: { "content-type": "text/event-stream" } });
    }
    return json(messages[messages.length - 1]);
  }

  private tool(name: string, args: Answer, workspace: string | null): Answer | null {
    if (workspace === null) {
      return this.account(name, args);
    }
    switch (name) {
      case "tilo_ls":
        return this.ls(args);
      case "tilo_read":
        return this.read(args);
      case "tilo_find":
        return this.find(args);
      case "tilo_do":
        return this.run(String(args.path ?? ""), (args.args ?? {}) as Answer);
      case "tilo_status":
        return this.status(args);
      case "tilo_watch":
        return this.watch(args);
      case "tilo":
        return this.program(args);
      case "tilo_get":
        return this.get(args);
      default:
        return null;
    }
  }

  private get(args: Answer): Answer {
    const content = String(args.content ?? "");
    const data = this.contents.get(content);
    if (data === undefined) {
      return this.failed("", "not_found", `the workspace has no ${content}`, "upload it first");
    }
    const offset = Number(args.offset ?? 0);
    const piece = data.subarray(offset, offset + this.piece);
    return {
      content,
      size: data.length,
      offset,
      length: piece.length,
      bytes: btoa(String.fromCharCode(...piece)),
    };
  }

  private account(name: string, args: Answer): Answer | null {
    if (name === "tilo_ls") {
      return {
        path: String(args.path ?? "account"),
        kind: "account.Account",
        rows: [
          { path: "account/workspaces", name: "workspaces", label: "Workspaces", kind: "account.Workspaces", has_children: true, actions: [] },
          { path: "account/keys", name: "keys", label: "Keys", kind: "account.Keys", has_children: true, actions: [] },
        ],
        actions: [],
      };
    }
    if (name === "tilo_do") {
      return this.accountDo(String(args.path ?? ""), (args.args ?? {}) as Answer);
    }
    if (name === "tilo_status") {
      return this.accountStatus(String(args.invocation_id ?? ""));
    }
    return null;
  }

  private accountDo(path: string, args: Answer): Answer {
    switch (path) {
      case "account/workspaces#list":
        return { status: "ok", path, result: { workspaces: this.workspaces, hosts: this.hosts } };
      case "account/workspaces#create": {
        const created = {
          id: "99999999-8888-7777-6666-555555555555",
          name: String(args.name ?? ""),
          host: String(args.host ?? "eu1"),
        };
        this.workspaces.push(created);
        return { status: "ok", path, result: { workspace: created } };
      }
      case "account/workspaces#delete":
        return this.failed(path, "not_supported", "the host cannot delete a workspace yet", "revoke the keys that reach it with account/keys#revoke");
      case "account/keys#list":
        return { status: "ok", path, result: { keys: this.keys } };
      case "account/keys#create":
        this.asks["key-abc"] = { status: "waiting" };
        return {
          status: "pending",
          path,
          invocation_id: "key-abc",
          ask: { invocation_id: "key-abc", url: "https://mcp.tilo.sh/keys/approve/abc" },
        };
      case "account/keys#mint":
        return {
          status: "ok",
          path,
          result: {
            grant: "g-minted",
            name: args.name ?? "minted",
            token: "tilo_sk_minted",
            workspace: args.workspace,
            mcp_url: `https://mcp.tilo.sh/w/${String(args.workspace)}/mcp`,
            expires_at_ms: 9_000_000_000_000,
          },
        };
      case "account/keys#revoke":
        return { status: "ok", path, result: { revoked: args.grant } };
      default:
        return this.failed(path, "not_found", `the account has no action ${path}`, "tilo_ls account lists its actions");
    }
  }

  private accountStatus(invocation: string): Answer {
    const asked = this.asks[invocation];
    if (asked === undefined) {
      return this.failed("account/keys#create", "expired", "nothing waits under this id", "ask again with account/keys#create");
    }
    if (asked.status === "waiting") {
      return {
        status: "pending",
        path: "account/keys#create",
        invocation_id: invocation,
        ask: { invocation_id: invocation, url: "https://mcp.tilo.sh/keys/approve/abc" },
      };
    }
    return {
      status: "ok",
      path: "account/keys#create",
      result: {
        key: "tilo_sk_created",
        grant: "g-created",
        name: "cli",
        scope: "account",
        expires_at_ms: 9_000_000_000_000,
      },
    };
  }

  private ls(args: Answer): Answer {
    const path = args.path as string | undefined;
    if (path === undefined) {
      return {
        path: "apps",
        rows: [
          { path: "notes", name: "notes", label: "Notes", kind: "app", state: "running", has_children: true, actions: [] },
          { path: "pdf-viewer", name: "pdf-viewer", label: "PDF Viewer", kind: "app", state: "installed", has_children: true, actions: [] },
          { path: "terminal", name: "terminal", label: "Terminal", kind: "app", state: "service", has_children: true, actions: [] },
        ],
        actions: [],
      };
    }
    if (path === "notes") {
      return {
        path,
        kind: "notes",
        rows: [{ path: TODO, name: "Todo", label: "Todo", kind: "notes.Page", has_children: true, actions: [] }],
        actions: [],
      };
    }
    const rows = [
      { path: `${TODO}/c01e`, name: "Buy milk", label: "Buy milk", kind: "notes.Block", has_children: false, actions: [{ id: "toggle-checkbox", label: "Done", kind: "undoable", params: [{ name: "id", ty: "Text", required: true }] }] },
      { path: `${TODO}/c02f`, name: "Buy bread", label: "Buy bread", kind: "notes.Block", has_children: false, actions: [] },
    ];
    const limit = args.limit as number | undefined;
    const cursor = args.cursor as string | undefined;
    if (limit !== undefined && cursor === undefined) {
      return { path, kind: this.kinds[path] ?? "notes.Page", rows: rows.slice(0, 1), actions: this.actionsOf(), next_cursor: "1" };
    }
    if (cursor !== undefined) {
      return { path, kind: this.kinds[path] ?? "notes.Page", rows: rows.slice(1), actions: this.actionsOf() };
    }
    return { path, kind: this.kinds[path] ?? "notes.Page", rows, actions: this.actionsOf() };
  }

  private actionsOf(): Answer[] {
    return [
      { id: "append-paragraph", label: "Add paragraph", kind: "undoable", params: [{ name: "text", ty: "Text", required: true }] },
      { id: "field-edit", label: "Edit a field", kind: "undoable", params: [{ name: "edit", ty: "Object", required: true }] },
    ];
  }

  private read(args: Answer): Answer {
    const path = String(args.path ?? "");
    if (!(path in this.fields)) {
      return this.failed(path, "not_found", `no resource ${path}`, "tilo_ls lists what is here");
    }
    return { path, kind: this.kinds[path] ?? "", fields: this.fields[path], children: [] };
  }

  private find(args: Answer): Answer {
    return {
      query: args.query,
      hits: [
        { path: TODO, kind: "notes.Page", label: "Todo", action: null },
        { path: `${TODO}#append-paragraph`, kind: "notes.Page", label: "Add paragraph", action: "append-paragraph" },
      ],
    };
  }

  private run(path: string, args: Answer): Answer {
    const invocation = `inv-${this.calls.length}`;
    if (path.endsWith("#field-edit")) {
      const edit = (args.edit ?? {}) as Answer;
      const resource = path.split("#")[0] as string;
      this.edits.push({ resource, edit });
      this.applyEdit(resource, edit);
      return { status: "ok", path, invocation_id: invocation, result: { edited: true }, resources: [resource] };
    }
    if (path.endsWith("#undo")) {
      return { status: "ok", path, invocation_id: invocation, result: { undone: (args.steps as number) ?? 1 } };
    }
    if (path === "workspace/members#invite") {
      const invited: Answer = {
        workspace: WORKSPACE_ID,
        invite: "ab".repeat(32),
        role: args.role,
        link: "https://tilo.sh/i/AAAAAAAAAAAAAAAA#key",
        expires_at_ns: 7,
      };
      if (args.email !== undefined) {
        Object.assign(invited, { email: args.email, sent: false, next: "send the link yourself" });
      }
      return { status: "ok", path, result: invited };
    }
    if (path === "workspace/members#list") {
      const members = [{ person: "cd".repeat(32), name: "Rosa Ortiz", role: "editor", administrator: false }];
      return { status: "ok", path, result: { workspace: WORKSPACE_ID, members, invitations: [] } };
    }
    if (path === `${TODO}#append-paragraph`) {
      if (this.appendPending) {
        return { status: "pending", path, invocation_id: invocation };
      }
      return { status: "ok", path, invocation_id: invocation, result: { appended: "c01e" }, resources: [TODO] };
    }
    if (path === "word-processor#export-content") {
      const letter = new TextEncoder().encode("PK cover letter");
      const content = `tilo.content:${letter.length}`;
      this.contents.set(content, letter);
      return { status: "ok", path, invocation_id: invocation, resources: [content] };
    }
    if (path === `${PDF}#import-pdf-content`) {
      return { status: "ok", path, invocation_id: invocation, result: { imported: args.content } };
    }
    if (path === `${PDF}#read-form-fields`) {
      return { status: "ok", path, invocation_id: invocation, result: { fields: Object.keys(this.fields[PDF]?.form_values ?? {}) } };
    }
    if (path === `${TODO}#ask`) {
      return {
        status: "pending",
        path,
        invocation_id: "ask-1",
        ask: { invocation_id: "ask-1", url: "https://mcp.tilo.sh/ask/1" },
      };
    }
    return this.failed(path, "invalid_arguments", `no action ${path}`, `tilo_ls ${path.split("#")[0]} lists its actions`);
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

  private status(args: Answer): Answer {
    const invocation = String(args.invocation_id ?? "");
    if (args.cancel) {
      return { status: "ok", path: `${TODO}#append-paragraph`, result: { cancelled: invocation } };
    }
    if (invocation === "ask-1") {
      if (!this.askAnswered) {
        return {
          status: "pending",
          path: `${TODO}#ask`,
          invocation_id: invocation,
          ask: { invocation_id: invocation, url: "https://mcp.tilo.sh/ask/1" },
        };
      }
      return { status: "ok", path: `${TODO}#ask`, invocation_id: invocation, result: { allowed: true } };
    }
    if (invocation in this.finished) {
      return {
        status: "ok",
        path: `${TODO}#append-paragraph`,
        invocation_id: invocation,
        result: this.finished[invocation],
      };
    }
    this.pollsUntilDone -= 1;
    if (this.pollsUntilDone < 0) {
      return {
        status: "ok",
        path: `${TODO}#append-paragraph`,
        invocation_id: invocation,
        result: { appended: "c01e" },
      };
    }
    return { status: "pending", path: `${TODO}#append-paragraph`, invocation_id: invocation };
  }

  private watch(args: Answer): Answer {
    const cursor = Number(args.cursor ?? 0);
    const events = this.events.slice(cursor, this.watchPage === null ? undefined : cursor + this.watchPage);
    return { events, cursor: String(cursor + events.length) };
  }

  private program(args: Answer): Answer {
    const program = String(args.program ?? "");
    if (program === "boom") {
      return {
        status: "failed",
        output: [],
        changed: 0,
        error: { code: "compile_error", message: "unknown name `boom`", next_steps: ["tilo_ls notes names its resources"] },
      };
    }
    if (program === "preview me") {
      return { status: "preview", output: ["4 changes in notes"], changed: 4, steps: [] };
    }
    return {
      status: "ok",
      output: ['n1 "Q3 plan"', "1 change in notes"],
      changed: 1,
      steps: [{ number: 1, apps: ["notes"], changed: 1 }],
    };
  }

  private failed(path: string, code: string, message: string, next: string): Answer {
    return { status: "failed", path, error: { code, message, next_steps: [next] } };
  }
}

function json(body: unknown, status = 200, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}
