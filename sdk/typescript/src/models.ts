/** The values the SDK answers: workspaces, keys, listings, hits and events. */

export interface Host {
  name: string;
  default: boolean;
}

export interface WorkspaceInfo {
  id: string;
  name: string;
  host: string;
  /** The part the person takes in it: `owner` for one of their own, else the
   * role they hold there (`viewer`, `commenter`, `editor`, `moderator`). */
  role: string;
}

export class WorkspaceList {
  readonly workspaces: WorkspaceInfo[];
  readonly hosts: Host[];

  constructor(raw: { workspaces?: unknown[]; hosts?: unknown[] } = {}) {
    this.workspaces = (raw.workspaces ?? []).map((workspace) => {
      const row = workspace as Record<string, unknown>;
      return {
        id: String(row.id ?? ""),
        name: String(row.name ?? ""),
        host: String(row.host ?? ""),
        role: String(row.role ?? ""),
      };
    });
    this.hosts = (raw.hosts ?? []).map((host) => {
      const row = host as Record<string, unknown>;
      return { name: String(row.name ?? ""), default: Boolean(row.default) };
    });
  }

  /** The workspace an id or a name names, names case-insensitively. */
  named(idOrName: string): WorkspaceInfo | undefined {
    const byId = this.workspaces.find((workspace) => workspace.id === idOrName);
    if (byId !== undefined) {
      return byId;
    }
    const folded = idOrName.toLowerCase();
    return this.workspaces.find((workspace) => workspace.name.toLowerCase() === folded);
  }

  [Symbol.iterator](): Iterator<WorkspaceInfo> {
    return this.workspaces[Symbol.iterator]();
  }

  get length(): number {
    return this.workspaces.length;
  }
}

/**
 * A key as the account lists it: its expiry is `null` for a key that lasts
 * until revoked; it says when it was last used and which device of the
 * person approved it.
 */
export interface Key {
  grant: string;
  name: string;
  scope: string;
  kind: string;
  createdAtMs: number | null;
  expiresAtMs: number | null;
  token: string;
  lastUsedAtMs: number | null;
  createdBy: string | null;
}

export function keyOf(raw: Record<string, unknown>): Key {
  return {
    grant: String(raw.grant ?? ""),
    name: String(raw.name ?? ""),
    scope: String(raw.scope ?? ""),
    kind: String(raw.kind ?? ""),
    createdAtMs: intOf(raw.created_at_ms),
    expiresAtMs: intOf(raw.expires_at_ms),
    token: String(raw.token ?? raw.key ?? ""),
    lastUsedAtMs: intOf(raw.last_used_at_ms),
    createdBy: typeof raw.created_by === "string" ? raw.created_by : null,
  };
}

export function intOf(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

export function textOf(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

export interface MintedKey {
  grant: string;
  token: string;
  name: string;
  workspace: string;
  mcpUrl: string;
  expiresAtMs: number | null;
}

export function mintedKeyOf(raw: Record<string, unknown>): MintedKey {
  return {
    grant: String(raw.grant ?? ""),
    token: String(raw.token ?? ""),
    name: String(raw.name ?? ""),
    workspace: String(raw.workspace ?? ""),
    mcpUrl: String(raw.mcp_url ?? ""),
    expiresAtMs: intOf(raw.expires_at_ms),
  };
}

/**
 * An invitation into a workspace: the link that brings one person in.
 *
 * Following `link` signs them in (with a passkey, or by approving an agent)
 * and lands them in the workspace as `role`. The door sends no mail: with an
 * `email`, `sent` is false and `next` says to hand the link over.
 */
export interface Invitation {
  link: string;
  role: string;
  workspace: string;
  invite: string;
  expiresAtNs: number | null;
  email: string | null;
  sent: boolean;
  next: string | null;
}

export function invitationOf(raw: Record<string, unknown>): Invitation {
  return {
    link: String(raw.link ?? ""),
    role: String(raw.role ?? ""),
    workspace: String(raw.workspace ?? ""),
    invite: String(raw.invite ?? ""),
    expiresAtNs: intOf(raw.expires_at_ns),
    email: textOf(raw.email),
    sent: raw.sent === true,
    next: textOf(raw.next),
  };
}

/** A person in a workspace, with their role there. */
export interface Member {
  person: string;
  role: string;
  name: string | null;
  administrator: boolean;
}

export function memberOf(raw: Record<string, unknown>): Member {
  return {
    person: String(raw.person ?? ""),
    role: String(raw.role ?? ""),
    name: textOf(raw.name),
    administrator: raw.administrator === true,
  };
}

export interface Row {
  path: string;
  name: string;
  label: string;
  kind: string;
  hasChildren: boolean;
  state: string | null;
  actions: unknown[];
  raw: Record<string, unknown>;
}

export function rowOf(raw: Record<string, unknown>): Row {
  return {
    path: String(raw.path ?? ""),
    name: String(raw.name ?? ""),
    label: String(raw.label ?? ""),
    kind: String(raw.kind ?? ""),
    hasChildren: Boolean(raw.has_children),
    state: textOf(raw.state),
    actions: Array.isArray(raw.actions) ? raw.actions : [],
    raw,
  };
}

/** What `tilo_ls` answered: a resource's rows and actions, and a cursor. */
export class Listing {
  readonly path: string;
  readonly kind: string;
  readonly rows: Row[];
  readonly actions: unknown[];
  readonly nextCursor: string | null;
  #more: ((cursor: string) => Promise<Listing>) | null;

  constructor(
    raw: Record<string, unknown> = {},
    more: ((cursor: string) => Promise<Listing>) | null = null,
  ) {
    this.path = String(raw.path ?? "");
    this.kind = String(raw.kind ?? "");
    this.rows = Array.isArray(raw.rows)
      ? raw.rows.map((row) => rowOf(row as Record<string, unknown>))
      : [];
    this.actions = Array.isArray(raw.actions) ? raw.actions : [];
    this.nextCursor = textOf(raw.next_cursor);
    this.#more = more;
  }

  /** The next page, or `null` when the listing is whole. */
  async more(): Promise<Listing | null> {
    if (this.#more === null || this.nextCursor === null) {
      return null;
    }
    return this.#more(this.nextCursor);
  }

  async *[Symbol.asyncIterator](): AsyncIterator<Row> {
    let page: Listing | null = this;
    while (page !== null) {
      yield* page.rows;
      page = page.nextCursor === null ? null : await page.more();
    }
  }
}

export interface Hit {
  path: string;
  kind: string;
  label: string;
  action: string | null;
}

export function hitOf(raw: Record<string, unknown>): Hit {
  return {
    path: String(raw.path ?? ""),
    kind: String(raw.kind ?? ""),
    label: String(raw.label ?? ""),
    action: textOf(raw.action),
  };
}

export interface Person {
  who: string;
  name: string;
  kind: string;
}

/** One thing `tilo_watch` told: a change, a run starting or finishing, presence. */
export interface Event {
  kind: string;
  path: string | null;
  change: string | null;
  sentence: string | null;
  by: string | null;
  run: string | null;
  runId: string | null;
  phase: string | null;
  invocationId: string | null;
  timeMs: number | null;
  undone: boolean;
  here: Person[];
  raw: Record<string, unknown>;
}

export function eventOf(raw: Record<string, unknown>): Event {
  return {
    kind: String(raw.kind ?? ""),
    path: textOf(raw.path),
    change: textOf(raw.change),
    sentence: textOf(raw.sentence),
    by: textOf(raw.by),
    run: textOf(raw.run),
    runId: textOf(raw.run_id),
    phase: textOf(raw.phase),
    invocationId: textOf(raw.invocation_id),
    timeMs: intOf(raw.time_ms),
    undone: Boolean(raw.undone),
    here: Array.isArray(raw.here)
      ? raw.here.map((person) => {
          const row = person as Record<string, unknown>;
          return {
            who: String(row.who ?? ""),
            name: String(row.name ?? ""),
            kind: String(row.kind ?? ""),
          };
        })
      : [],
    raw,
  };
}

/** What the `tilo` tool answered: a program's output and the steps it took. */
export interface ProgramAnswer {
  status: string;
  output: string[];
  changed: number;
  steps: unknown[];
  raw: Record<string, unknown>;
}

export function programAnswerOf(raw: Record<string, unknown>): ProgramAnswer {
  return {
    status: String(raw.status ?? ""),
    output: Array.isArray(raw.output) ? raw.output.map(String) : [],
    changed: intOf(raw.changed) ?? 0,
    steps: Array.isArray(raw.steps) ? raw.steps : [],
    raw,
  };
}
