/** The values the SDK answers: workspaces, keys, listings, invocations, hits and events. */

/** One of the person's workspaces, as the account lists them. */
export interface WorkspaceInfo {
  id: string;
  name: string;
  /** The host that keeps it: a relay's name, or the device that hosts it. */
  host: string;
  /** The part the person takes in it: `owner` for one of their own, else the
   * role they hold there (`viewer`, `commenter`, `editor`, `moderator`). */
  role: string;
  /** What the person labelled it with when they made it (`customer_id` →
   * `cus_8121`): read by their own keys alone, empty for none. */
  labels: Record<string, string>;
  /** Whether the person let the keys made for it alone run commands in its
   * Linux machine without asking them each time. */
  unattended: boolean;
}

/** Everything a workspace holds, as `workspace.export()` reads it. */
export interface WorkspaceExport {
  workspace: WorkspaceInfo;
  exportedAtMs: number;
  /** Every resource of every app, each as a read answers it. */
  resources: Record<string, unknown>[];
  /** What did not read, each with why. */
  unread: { path: string; error: string }[];
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
    labels: labelsOf(raw.labels),
    unattended: raw.unattended === true,
  };
}

function labelsOf(raw: unknown): Record<string, string> {
  const labels: Record<string, string> = {};
  if (raw !== null && typeof raw === "object") {
    for (const [name, value] of Object.entries(raw)) {
      if (typeof value === "string") {
        labels[name] = value;
      }
    }
  }
  return labels;
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

/**
 * A key as the workspace that admitted it keeps its grant: `scope` the workspace it reaches
 * alone, `account` for every one of the person's.
 */
export function keyGranted(grant: Record<string, unknown>): Key {
  return keyOf({
    grant: grant.id,
    name: grant.name,
    scope: grant.scope ?? "account",
    kind: grant.parent == null ? "key" : "minted",
    created_at_ms: grant.created_ms,
    expires_at_ms: grant.expires_ms,
  });
}

export function intOf(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

export function textOf(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

/** An object's fields, or an empty one when the value is no object. */
export function recordOf(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

/** The objects in a value's array, each once, without the rest. */
export function arrayOf(value: unknown): Record<string, unknown>[] {
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
  /** The resource's own id, which names it whatever else is titled the same. */
  id: string | null;
  /** The app's stable key for it, the name its actions answer it by. */
  key: string | null;
  label: string;
  kind: string;
  hasChildren: boolean;
  /** For an app: `running`, `service` or `installed`. */
  state: string | null;
  actions: Action[];
  raw: Record<string, unknown>;
}

/**
 * A listing's row, its actions as the listing describes them: a row names
 * the ones it offers by id, and the listing's `kinds` describes each kind's
 * once.
 */
export function rowOf(
  raw: Record<string, unknown>,
  kinds: Record<string, unknown> = {},
): Row {
  const kind = String(raw.kind ?? "");
  const described = arrayOf(kinds[kind]).map(actionOf);
  const actions = (Array.isArray(raw.actions) ? raw.actions : []).flatMap(
    (entry: unknown): Action[] => {
      if (typeof entry === "string") {
        return described.filter((action) => action.id === entry);
      }
      return entry !== null && typeof entry === "object"
        ? [actionOf(entry as Record<string, unknown>)]
        : [];
    },
  );
  return {
    path: String(raw.path ?? ""),
    name: String(raw.name ?? ""),
    id: textOf(raw.id),
    key: textOf(raw.key),
    label: String(raw.label ?? ""),
    kind,
    hasChildren: raw.has_children === true,
    state: textOf(raw.state),
    actions,
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
    const kinds =
      raw.kinds !== null && typeof raw.kinds === "object"
        ? (raw.kinds as Record<string, unknown>)
        : {};
    this.rows = arrayOf(raw.rows).map((row) => rowOf(row, kinds));
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

/**
 * One invocation of a workspace, as its listing names it: who made it, how it
 * stands, when, and what it changed. `id` is what
 * `new Invocation(await ws.endpoint(), id)` reads, cancels and undoes, while the
 * host remembers its calls; one known only from the workspace's records (the host
 * restarted since) has none, and is told apart by `step`, the undo step its
 * changes keep in each record's history.
 */
export interface InvocationInfo {
  id: string | null;
  /** Every call of it the host remembers, oldest first. */
  calls: string[];
  step: string | null;
  /** Who made it: the key's name, an agent's or a person's. */
  by: string;
  /** `agent` (every key is one), `person`, `bot` or `automation`. */
  byKind: string;
  /** What its client called it (`ws.invocation(title)`), when it did. */
  title: string | null;
  /** `waiting` (for the person), `running`, `ended`, `failed`, `cancelled` or `undone`. */
  status: string;
  /** Why it failed, typed: `confirmation_required` when the person did not allow it. */
  failure: string | null;
  startedMs: number;
  endedMs: number | null;
  /** What its last call asked for, `<app>#<action>`, while the host remembers it. */
  action: string | null;
  /** What its newest change did, without the values it carried: the action by its published label. */
  summary: string | null;
  /** How many changes it made to the workspace's records. */
  changes: number;
  /** How many of its edits were taken back since. */
  undone: number;
  /** The records it changed: `{app, app_id, record}`. */
  records: Record<string, unknown>[];
  /** Its edits still stand and nobody's edit stands on them. */
  undoable: boolean;
  /** Why it would not be taken back, when someone's edit stands on its own. */
  undoBlocked: string | null;
  /** The workspace's records keep it: it is listed after the host restarts. */
  recorded: boolean;
  /** The host remembers its calls: `id` reads, stops and takes it back. */
  remembered: boolean;
  /** The resource the call was made from (`kind`, `id`), when it named one: whoever may write
   * it may stop the run or run it again. */
  origin: Record<string, unknown> | null;
  /** The invocation this one runs again, when it is a retry. */
  retries: string | null;
  /** What a model call it made cost: `provider`, `model`, `input_tokens`, `output_tokens` and
   * `cost_micros`. */
  spend: Record<string, unknown> | null;
  raw: Record<string, unknown>;
}

export function invocationInfoOf(raw: Record<string, unknown>): InvocationInfo {
  const by = raw.by !== null && typeof raw.by === "object" ? (raw.by as Record<string, unknown>) : {};
  return {
    id: textOf(raw.id),
    calls: Array.isArray(raw.calls) ? raw.calls.filter((call): call is string => typeof call === "string") : [],
    step: textOf(raw.step),
    by: String(by.name ?? ""),
    byKind: String(by.kind ?? ""),
    title: textOf(raw.title),
    status: String(raw.status ?? ""),
    failure: textOf(raw.failure),
    startedMs: intOf(raw.started_ms) ?? 0,
    endedMs: intOf(raw.ended_ms),
    action: textOf(raw.action),
    summary: textOf(raw.summary),
    changes: intOf(raw.changes) ?? 0,
    undone: intOf(raw.undone) ?? 0,
    records: arrayOf(raw.records),
    undoable: raw.undoable === true,
    undoBlocked: textOf(raw.undo_blocked),
    recorded: raw.recorded === true,
    remembered: raw.remembered === true,
    origin: raw.origin !== null && typeof raw.origin === "object" ? (raw.origin as Record<string, unknown>) : null,
    retries: textOf(raw.retries),
    spend: raw.spend !== null && typeof raw.spend === "object" ? (raw.spend as Record<string, unknown>) : null,
    raw,
  };
}

/** A page of a workspace's invocations, newest first, with the totals; `for await`
 * reads on to the oldest. */
export class InvocationList {
  readonly invocations: InvocationInfo[];
  /** Invocations the workspace holds, of which `invocations` is a page. */
  readonly total: number;
  /** Of those, the ones waiting for the person. */
  readonly waiting: number;
  /** A record could not be read yet: invocations that changed only it are missing. */
  readonly pending: boolean;
  readonly nextCursor: string | null;
  /** The page as a person reads it, one line per invocation. */
  readonly text: string;
  #more: ((cursor: string) => Promise<InvocationList>) | null;

  constructor(
    raw: Record<string, unknown> = {},
    more: ((cursor: string) => Promise<InvocationList>) | null = null,
  ) {
    this.invocations = arrayOf(raw.items).map(invocationInfoOf);
    this.total = intOf(raw.total) ?? 0;
    this.waiting = intOf(raw.waiting) ?? 0;
    this.pending = raw.pending === true;
    this.nextCursor = textOf(raw.next_cursor);
    this.text = textOf(raw.text) ?? "";
    this.#more = more;
  }

  /** The next page, older, or `null` on the last. */
  async more(): Promise<InvocationList | null> {
    if (this.#more === null || this.nextCursor === null) {
      return null;
    }
    return this.#more(this.nextCursor);
  }

  async *[Symbol.asyncIterator](): AsyncIterator<InvocationInfo> {
    let page: InvocationList | null = this;
    while (page !== null) {
      yield* page.invocations;
      page = page.nextCursor === null ? null : await page.more();
    }
  }
}

/**
 * What an app's window shows for a resource: the view's semantic lines —
 * every label, value and control in reading order — the same tree, and a
 * picture of it when one was asked for.
 */
export interface View {
  /** The lines, one node each: `<role> "<name>" = "<value>" [state]`, indented. */
  text: string;
  path: string;
  /** The app whose window shows it. */
  app: string;
  /** `resource` when the window shows the resource, `window` when it could not. */
  shown: string;
  /** The same view as a tree: role, name, value, state, bounds and action of each node. */
  tree: unknown[];
  nodes?: number;
  omitted?: number;
  viewport?: [number, number];
  window_origin?: [number, number];
  revision?: string;
  /** A `tilo.content:<hash>` of the picture, when one was asked for. */
  picture?: string;
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

export function personOf(raw: Record<string, unknown>): Person {
  return {
    who: String(raw.who ?? ""),
    name: String(raw.name ?? ""),
    kind: String(raw.kind ?? ""),
  };
}

/**
 * What a change did to the resource it changed: `created` made it, `edited`
 * changed it, and `removed` took it away. The store reads it from the
 * recorded change; no caller works it out from what it saw before.
 */
export type EventWhat = "created" | "edited" | "removed";

/** `value` as an {@link EventWhat}, or null for anything else. */
export function eventWhatOf(value: unknown): EventWhat | null {
  return value === "created" || value === "edited" || value === "removed" ? value : null;
}

/**
 * One thing the workspace's stream told. `cursor` is where the stream has read
 * up to after it: reconnecting with it resumes after this event.
 */
export interface StreamEvent {
  cursor: string | null;
  path: string | null;
  change: string | null;
  sentence: string | null;
  /** Who made the change. */
  by: string | null;
  /** The action the change was made by (`ban`, `new_channel`), as its author stamped it; null
   * for a change that names none. Its arguments never travel here. */
  verb: string | null;
  /** The records a change wrote: each with its `path`, and its `app`,
   * `record` and `item` where the change names them. */
  resources: Record<string, unknown>[];
  /** Who made the change, structured: `kind` (`person` or `agent`) and
   * `name`, and the actor's own fields besides. */
  actor: Record<string, unknown> | null;
  /** The invocation the event is of. */
  invocation: string | null;
  /** The group's title, as its first call named it. */
  title: string | null;
  /** The group a step belongs to, for a grouped invocation. */
  group: string | null;
  /** `started`, `succeeded`, `failed` or `cancelled`. */
  phase: string | null;
  timeMs: number | null;
  undone: boolean;
  here: Person[];
  raw: Record<string, unknown>;
}

/** A change to a resource, and what it did to it. */
export interface ChangeEvent extends StreamEvent {
  kind: "change";
  /** What the change did to its resource: it made it, edited it or removed it. */
  what: EventWhat;
}

/**
 * Anything else the stream told: an invocation's progress, who is here, or the
 * workspace's own state. These are not resource changes, and say no `what`.
 */
export interface OtherEvent extends StreamEvent {
  kind: "invocation" | "presence" | "workspace" | "sync" | "unwatched";
}

/** One thing the workspace's stream told. */
export type Event = ChangeEvent | OtherEvent;

/** An item a write made: its id, and the address it is published under. */
export interface Made {
  id: string;
  address: string;
}

/**
 * An action's answer, and an invocation's: what it did, what it changed, and the link
 * to the one record a person opens it with.
 */
export interface ActionAnswer {
  /** `ok`, `pending` or `cancelled`. */
  status: string;
  /** The invocation: what `…/invocations/{invocation}` follows. */
  invocation: string | null;
  /** The action at the resource it resolved to, `app/resource#action`. */
  path: string | null;
  result: unknown;
  /** The resources it touched, `tilo.content:` for the files it made. */
  resources: string[];
  /** What it made, each with its id and the address it is published under. */
  made: Made[];
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
    invocation: textOf(raw.invocation),
    path: textOf(raw.path),
    result: raw.result ?? null,
    resources: Array.isArray(raw.resources) ? raw.resources.map(String) : [],
    made: Array.isArray(raw.made)
      ? (raw.made as Made[]).map((item) => ({id: String(item.id), address: String(item.address)}))
      : [],
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
 * instead (Writer's `export_document`), that file's `tilo.content:` reference,
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
  /** `ok`, `waiting` (for its person's yes: `text` names where they answer) or `failed`. */
  status: string;
  output: string[];
  changed: number;
  steps: unknown[];
  /** What a person reads, each resource by its reference (`n3`). */
  text: string;
  /** The program session it ran in: pass it to the next program to continue it. */
  session: string;
  raw: Record<string, unknown>;
}

export function programAnswerOf(raw: Record<string, unknown>): ProgramAnswer {
  return {
    status: String(raw.status ?? ""),
    output: Array.isArray(raw.output) ? raw.output.map(String) : [],
    changed: intOf(raw.changed) ?? 0,
    steps: Array.isArray(raw.steps) ? raw.steps : [],
    text: String(raw.text ?? ""),
    session: String(raw.session ?? ""),
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
  const subscription = recordOf(raw.subscription);
  const usage = recordOf(subscription.usage);
  const workspaces = arrayOf(usage.workspaces).map((row) => ({
    workspace: String(row.workspace ?? ""),
    vmSeconds: intOf(row.vm_seconds) ?? 0,
    running: row.running === true,
    storedBytes: intOf(row.stored_bytes) ?? 0,
    raw: row,
  }));
  return {
    host: String(raw.host ?? ""),
    plan: textOf(subscription.plan),
    workspaces,
    vmSeconds: workspaces.reduce((total, row) => total + row.vmSeconds, 0),
    running: workspaces.filter((row) => row.running).map((row) => row.workspace),
    raw,
  };
}

