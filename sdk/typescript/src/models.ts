/** The values the SDK answers: workspaces, keys, listings, runs, hits and events. */

/** One of the person's workspaces, as the account lists them. */
export interface WorkspaceInfo {
  id: string;
  name: string;
  /** The host that keeps it: a relay's name, or the device that hosts it. */
  host: string;
  /** The part the person takes in it: `owner` for one of their own, else the
   * role they hold there (`viewer`, `commenter`, `editor`, `moderator`). */
  role: string;
}

/** The account's workspaces. */
export class WorkspaceList {
  readonly workspaces: WorkspaceInfo[];

  constructor(raw: { workspaces?: unknown[] } = {}) {
    this.workspaces = (raw.workspaces ?? [])
      .filter((workspace): workspace is Record<string, unknown> => workspace !== null && typeof workspace === "object")
      .map(workspaceOf);
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

  /** How many workspaces it lists. */
  get length(): number {
    return this.workspaces.length;
  }
}

export function workspaceOf(raw: Record<string, unknown>): WorkspaceInfo {
  return {
    id: String(raw.id ?? ""),
    name: String(raw.name ?? ""),
    host: String(raw.host ?? ""),
    role: String(raw.role ?? "owner"),
  };
}

/**
 * A key as the account lists it: its expiry is `null` for a key that lasts
 * until revoked; it says when it was last used and which device of the
 * person approved it.
 */
export interface Key {
  grant: string;
  name: string;
  /** `all` or the one workspace it reaches. */
  scope: string;
  /** `agent`, `key` or `minted`. */
  kind: string;
  createdAtMs: number | null;
  expiresAtMs: number | null;
  /** The key itself, for a key just approved or minted; empty when listed. */
  token: string;
  lastUsedAtMs: number | null;
  createdBy: string | null;
  /** Whether it may operate web pages in its workspace's Browser. */
  browserPages: boolean;
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
    createdBy: textOf(raw.created_by),
    browserPages: raw.browser_pages === true,
  };
}

export function intOf(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

export function textOf(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function arrayOf(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value)
    ? value.filter((row): row is Record<string, unknown> => row !== null && typeof row === "object")
    : [];
}

/** A key just minted: the token to use, and where it points. */
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
 * and lands them in the workspace as `role`. The host sends no mail: with an
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

/** One parameter of an action, as its app publishes it. */
export interface Param {
  name: string;
  ty: string;
  required: boolean;
  description: string | null;
  choices: unknown[] | null;
}

export function paramOf(raw: Record<string, unknown>): Param {
  return {
    name: String(raw.name ?? ""),
    ty: String(raw.ty ?? ""),
    required: raw.required === true,
    description: textOf(raw.description),
    choices: Array.isArray(raw.choices) ? raw.choices : null,
  };
}

/** An action a resource offers: what running it takes, and what it may reach. */
export interface Action {
  id: string;
  label: string;
  /** `read`, `undoable` or `outside`. */
  kind: string;
  params: Param[];
  raw: Record<string, unknown>;
}

export function actionOf(raw: Record<string, unknown>): Action {
  return {
    id: String(raw.id ?? ""),
    label: String(raw.label ?? ""),
    kind: String(raw.kind ?? ""),
    params: arrayOf(raw.params).map(paramOf),
    raw,
  };
}

/** One row of a listing: a resource under the path listed, or an app. */
export interface Row {
  path: string;
  name: string;
  label: string;
  kind: string;
  hasChildren: boolean;
  /** For an app: `running`, `service` or `installed`. */
  state: string | null;
  actions: Action[];
  raw: Record<string, unknown>;
}

export function rowOf(raw: Record<string, unknown>): Row {
  return {
    path: String(raw.path ?? ""),
    name: String(raw.name ?? ""),
    label: String(raw.label ?? ""),
    kind: String(raw.kind ?? ""),
    hasChildren: raw.has_children === true,
    state: textOf(raw.state),
    actions: arrayOf(raw.actions).map(actionOf),
    raw,
  };
}

/** A listing: what is under a path, the path's own actions, and a cursor. */
export class Listing {
  readonly path: string;
  readonly kind: string;
  readonly rows: Row[];
  readonly actions: Action[];
  readonly nextCursor: string | null;
  #more: ((cursor: string) => Promise<Listing>) | null;

  constructor(
    raw: Record<string, unknown> = {},
    more: ((cursor: string) => Promise<Listing>) | null = null,
  ) {
    this.path = String(raw.path ?? "");
    this.kind = String(raw.kind ?? "");
    this.rows = arrayOf(raw.rows).map(rowOf);
    this.actions = arrayOf(raw.actions).map(actionOf);
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

/** One search hit: a path the other calls take, and what it offers. */
export interface Hit {
  path: string;
  kind: string;
  label: string;
  /** The action the hit runs; `open` when it opens the resource itself. */
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

/** Someone in the workspace: a person, or one of its agents. */
export interface Person {
  who: string;
  name: string;
  kind: string;
}

function personOf(raw: Record<string, unknown>): Person {
  return {
    who: String(raw.who ?? ""),
    name: String(raw.name ?? ""),
    kind: String(raw.kind ?? ""),
  };
}

/**
 * One thing the workspace's stream told: a change, a run starting or
 * finishing, or who is there. `cursor` is where the stream has read up to
 * after it: reconnecting with it resumes after this event.
 */
export interface Event {
  /** `change`, `run` or `presence`. */
  kind: string;
  cursor: string | null;
  path: string | null;
  change: string | null;
  sentence: string | null;
  /** Who made the change. */
  by: string | null;
  /** The run (an invocation id) the event is of. */
  run: string | null;
  /** The run's title, as its first call named it. */
  title: string | null;
  /** The run a step belongs to, for a grouped run. */
  group: string | null;
  /** `started`, `succeeded`, `failed` or `cancelled`. */
  phase: string | null;
  timeMs: number | null;
  undone: boolean;
  here: Person[];
  raw: Record<string, unknown>;
}

/** An event from a stream's frame: its kind, its JSON and its cursor. */
export function eventOf(kind: string | null, data: unknown, cursor: string | null = null): Event {
  const raw = data !== null && typeof data === "object" && !Array.isArray(data)
    ? (data as Record<string, unknown>)
    : {};
  const here = Array.isArray(raw.here)
    ? raw.here.filter((person): person is Record<string, unknown> => person !== null && typeof person === "object").map(personOf)
    : [];
  return {
    kind: kind ?? "change",
    cursor,
    path: textOf(raw.path),
    change: textOf(raw.change),
    sentence: textOf(raw.sentence),
    by: textOf(raw.by),
    run: textOf(raw.run),
    title: textOf(raw.title),
    group: textOf(raw.group),
    phase: textOf(raw.phase),
    timeMs: intOf(raw.time_ms),
    undone: raw.undone === true,
    here,
    raw,
  };
}

/**
 * An action's answer, and a run's: what it did, what it changed, and the link
 * to the one record a person opens it with.
 */
export interface ActionAnswer {
  /** `ok`, `pending` or `cancelled`. */
  status: string;
  /** The run: what `…/runs/{run}` follows, and what `Tilo-Run` groups. */
  run: string | null;
  /** The action at the resource it resolved to, `app/resource#action`. */
  path: string | null;
  result: unknown;
  /** The resources it touched, `tilo.content:` for the files it made. */
  resources: string[];
  link: string | null;
  /** Every record it changed, once each. */
  links: string[];
  /** What the person is being asked, while it waits for them. */
  ask: Record<string, unknown> | null;
  /** What actually ran, when the person changed the arguments first. */
  editedByPerson: unknown;
  undone: unknown;
  raw: Record<string, unknown>;
}

export function actionAnswerOf(raw: Record<string, unknown>): ActionAnswer {
  return {
    status: String(raw.status ?? ""),
    run: textOf(raw.run),
    path: textOf(raw.path),
    result: raw.result ?? null,
    resources: Array.isArray(raw.resources) ? raw.resources.map(String) : [],
    link: textOf(raw.link),
    links: Array.isArray(raw.links) ? raw.links.map(String) : [],
    ask:
      raw.ask !== null && typeof raw.ask === "object" && !Array.isArray(raw.ask)
        ? (raw.ask as Record<string, unknown>)
        : null,
    editedByPerson: raw.edited_by_person ?? null,
    undone: raw.undone ?? null,
    raw,
  };
}

const CONTENT_PREFIX = "tilo.content:";

/**
 * What an action answered: its value, or, when it gave none and made a file
 * instead (Writer's `export-content`), that file's `tilo.content:` reference,
 * which `ws.files.download` fetches. Several files answer a list.
 */
export function resultOf(answer: ActionAnswer): unknown {
  if (answer.result !== undefined && answer.result !== null) {
    return answer.result;
  }
  const made = answer.resources.filter((resource) => resource.startsWith(CONTENT_PREFIX));
  if (made.length === 1) {
    return made[0];
  }
  return made.length > 0 ? made : null;
}

/** What a tilolang program answered: its output, and the steps it took. */
export interface ProgramAnswer {
  /** `ok`, `preview` or `failed`. */
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

/** What one workspace keeps, and the VM time it ran. */
export interface WorkspaceUsage {
  workspace: string;
  vmSeconds: number;
  running: boolean;
  storedBytes: number;
  raw: Record<string, unknown>;
}

/** The person's account on their host: its plan, and per workspace what it keeps and ran. */
export interface Usage {
  host: string;
  plan: string | null;
  workspaces: WorkspaceUsage[];
  /** The VM seconds every workspace ran this billing period. */
  vmSeconds: number;
  /** The workspaces with a VM running now. */
  running: string[];
  raw: Record<string, unknown>;
}

export function usageOf(raw: Record<string, unknown>): Usage {
  const standing = recordOf(raw.standing);
  const usage = recordOf(standing.usage);
  const workspaces = arrayOf(usage.workspaces).map((row) => ({
    workspace: String(row.workspace ?? ""),
    vmSeconds: intOf(row.vm_seconds) ?? 0,
    running: row.running === true,
    storedBytes: intOf(row.stored_bytes) ?? 0,
    raw: row,
  }));
  return {
    host: String(raw.host ?? ""),
    plan: textOf(standing.plan),
    workspaces,
    vmSeconds: workspaces.reduce((total, row) => total + row.vmSeconds, 0),
    running: workspaces.filter((row) => row.running).map((row) => row.workspace),
    raw,
  };
}

function recordOf(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

/** What `GET /v1` answers: the API's version, and what the key reaches. */
export interface ApiInfo {
  apiVersion: string;
  server: string;
  key: Key | null;
  raw: Record<string, unknown>;
}

export function apiInfoOf(raw: Record<string, unknown>): ApiInfo {
  const key = recordOf(raw.key);
  return {
    apiVersion: String(raw.api_version ?? ""),
    server: String(raw.server ?? ""),
    key: Object.keys(key).length === 0 ? null : keyOf(key),
    raw,
  };
}

/** A file's bytes, as they went up. */
export interface Upload {
  /** `tilo.content:<hash>`: what an action takes the file as. */
  content: string;
  size: number;
}

export function uploadOf(raw: Record<string, unknown>): Upload {
  return { content: String(raw.content ?? ""), size: intOf(raw.size) ?? 0 };
}
