/**
 * A resource as a typed object: its fields, its actions, and its edits.
 *
 * A field reads from the resource's `fields`; a method call is the resource's
 * action (`readFormFields()` runs `read-form-fields`); assigning a name is a
 * field edit, through the generic `field-edit` action every resource
 * publishes. `form.formValues.surname = "Doe"` is the same edit one level
 * down.
 *
 * A resource made through generated types names its app's surface in every
 * call (`Tilo-Surface`): a workspace whose app publishes another surface
 * refuses the call (`surface_changed`) instead of running it.
 */

import { currentGroup, run, type Workspace } from "./client.ts";
import type { ActionAnswer, Event, Resource as Read, Row } from "./types/api-v1.ts";

/** The suffix a generated bulk call's method carries: `createMany`. */
const MANY_SUFFIX = "Many";
/** A bulk call's bounds: at most this many members, and about this much JSON, per call. */
const MANY_MEMBERS = 500;
const MANY_BYTES = 4 * 1024 * 1024;

/** One step of a field path: a field's name, a map key, or `{item: id}`. */
export type Step = string | Record<string, unknown>;

/** One item of a collection field, as its read lists it: the id an action
 * takes it by, and its value. `issues[0].id` names the issue `rename` takes,
 * and `issues[0].value.title` is its title. */
export interface Entry<T> {
  id: string;
  value: T;
}

/**
 * A resource as a typed object: its fields, its actions and its edits.
 *
 * A field reads as an attribute (`page.title`) and assigning one edits it
 * (`page.title = "Done"`); calling a method runs the resource's action of that
 * name. `load()` reads it and `flush()` waits for the edits on their way.
 */
export interface Resource {
  readonly path: string;
  readonly kind: string;
  /** The app's surface the types were made from, `<app>@<digest>`, when they were. */
  readonly surface: string | null;
  readonly fields: Record<string, unknown>;
  readonly children: NonNullable<Read["children"]>;
  /** The resource as the API answered its read. */
  readonly raw: Read | null;
  load(): Promise<Resource>;
  refresh(): Promise<Resource>;
  flush(): Promise<void>;
  action(name: string, args?: Record<string, unknown>): Promise<unknown>;
  /** Run an action for each of `items`, a few all-or-nothing calls at most. */
  many(action: string, items: Record<string, unknown>[]): Promise<ActionAnswer[]>;
  edit(steps: Step[], value: unknown, op?: string): Promise<unknown>;
  remove(...steps: Step[]): Promise<unknown>;
  watch(): AsyncIterableIterator<Event>;
  [name: string]: unknown;
}

/**
 * The surface a resource's calls name: generated types answer it as their
 * `static readonly surface`, and a caller may give the string itself.
 */
export type Surface = string | { surface?: string | null } | null;

/**
 * A reference to an item an earlier item of the same `many` call makes:
 * `{"$made": {"member": 0, "item": 0}}`, by the item's place in the call and
 * which of the ids it made (usually 0, as a create makes one). The call fills
 * the reference with that id as it applies, and the change keeps the
 * reference, so every device resolves it the same.
 */
export function made(member: number, item = 0): Record<string, Record<string, number>> {
  return { $made: { member, item } };
}

/** An action id from the name a caller writes: `readFormFields` is `read-form-fields`. */
export function kebab(name: string): string {
  let out = "";
  for (let index = 0; index < name.length; index += 1) {
    const character = name[index] as string;
    if (character === "_") {
      out += "-";
      continue;
    }
    const upper = character >= "A" && character <= "Z";
    if (!upper) {
      out += character;
      continue;
    }
    const previous = index > 0 ? (name[index - 1] as string) : "";
    const following = index + 1 < name.length ? (name[index + 1] as string) : "";
    const lower = (text: string) => text !== "" && text === text.toLowerCase();
    if (lower(previous) || (lower(following) && previous !== "" && previous === previous.toUpperCase())) {
      out += "-";
    }
    out += character.toLowerCase();
  }
  return out;
}

/** A field path as the field-edit action takes it. */
export function segments(steps: Step[]): Record<string, unknown>[] {
  if (steps.length === 0) {
    throw new TypeError("a field edit names a field");
  }
  return steps.map((step, index) => {
    if (typeof step === "string") {
      return index === 0 ? { field: step } : { key: step };
    }
    if (step && typeof step === "object") {
      return { ...step };
    }
    throw new TypeError(`a field path step is a name or a mapping, not ${String(step)}`);
  });
}

const OWN = new Set([
  "path",
  "kind",
  "surface",
  "fields",
  "children",
  "raw",
  "load",
  "refresh",
  "flush",
  "action",
  "edit",
  "remove",
  "watch",
  "has",
  "then",
]);

/** A map field: reading gives its values, assigning sends a field edit. */
function wrapMap(
  impl: ResourceImpl,
  steps: Step[],
  value: Record<string, unknown>,
): Record<string, unknown> {
  return new Proxy(value, {
    get(target, property) {
      if (typeof property !== "string") {
        return Reflect.get(target, property);
      }
      return wrap(impl, [...steps, property], target[property]);
    },
    set(target, property, next) {
      if (typeof property !== "string") {
        return Reflect.set(target, property, next);
      }
      impl.assign([...steps, property], next);
      target[property] = next;
      return true;
    },
    deleteProperty(target, property) {
      if (typeof property === "string") {
        impl.schedule([...steps, property], null, "remove-key");
      }
      return Reflect.deleteProperty(target, property);
    },
  });
}

/** What a bulk call is made of: one action's arguments. */
interface Member {
  action: string;
  args: Record<string, unknown>;
}

/** `members` in the calls a bulk call sends: at most `MANY_MEMBERS` and `MANY_BYTES`. */
function chunksOf(members: Member[]): Member[][] {
  const chunks: Member[][] = [];
  const encoder = new TextEncoder();
  let chunk: Member[] = [];
  let size = 0;
  for (const member of members) {
    const weight = encoder.encode(JSON.stringify(member)).length;
    if (chunk.length > 0 && (chunk.length >= MANY_MEMBERS || size + weight > MANY_BYTES)) {
      chunks.push(chunk);
      chunk = [];
      size = 0;
    }
    chunk.push(member);
    size += weight + 1;
  }
  if (chunk.length > 0) {
    chunks.push(chunk);
  }
  return chunks;
}

function wrap(impl: ResourceImpl, steps: Step[], value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map((item, index) => wrap(impl, [...steps, String(index)], item));
  }
  if (value !== null && typeof value === "object") {
    return wrapMap(impl, steps, value as Record<string, unknown>);
  }
  return value;
}

class ResourceImpl {
  readonly path: string;
  readonly workspace: Workspace;
  readonly surface: string | null;
  readonly #pending: Promise<unknown>[] = [];
  /** The action names the generated types declare, reachable before a read. */
  readonly #actions: Set<string>;
  /** Each field is written by its own setter, `set-<field>`, not by a field edit. */
  readonly #setters: boolean;
  #answer: Read | null = null;
  /** The proxy that fronts this implementation, which reads answer. */
  self: unknown = null;

  constructor(
    workspace: Workspace,
    path: string,
    surface: string | null = null,
    actions: readonly string[] = [],
    setters = false,
  ) {
    this.workspace = workspace;
    this.path = path;
    this.surface = surface;
    this.#actions = new Set(actions);
    this.#setters = setters;
  }

  get kind(): string {
    return String(this.#answer?.kind ?? "");
  }

  get raw(): Read | null {
    return this.#answer;
  }

  get children(): NonNullable<Read["children"]> {
    return this.#answer?.children ?? [];
  }

  get fields(): Record<string, unknown> {
    const fields = this.#answer?.fields;
    if (!fields || typeof fields !== "object") {
      return {};
    }
    const wrapped: Record<string, unknown> = {};
    for (const [name, value] of Object.entries(fields as Record<string, unknown>)) {
      wrapped[name] = wrap(this, [name], value);
    }
    return wrapped;
  }

  /** The wire's own key for a field: `formValues` and `form_values` are one. */
  wireKey(name: string): string {
    if (this.has(name)) {
      return name;
    }
    const snake = name.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase();
    return this.has(snake) ? snake : name;
  }

  has(name: string): boolean {
    const fields = this.#answer?.fields;
    return fields !== undefined && fields !== null && typeof fields === "object"
      ? name in (fields as Record<string, unknown>)
      : false;
  }

  async load(): Promise<unknown> {
    await this.flush();
    this.#answer = await this.workspace.readResource({ path: this.path }, this.#named());
    return this.self ?? this;
  }

  refresh(): Promise<unknown> {
    return this.load();
  }

  /** Await the edits that are on their way, throwing the first refusal. */
  async flush(): Promise<void> {
    while (this.#pending.length > 0) {
      const pending = this.#pending.splice(0, this.#pending.length);
      await Promise.all(pending);
    }
  }

  /** What every call of a typed resource names: the surface its types were made from. */
  #named(): Record<string, string> {
    return this.surface === null ? {} : { "tilo-surface": this.surface };
  }

  /** Run `action` here: what it produced, once it finished. */
  async #run(action: string, args: Record<string, unknown>): Promise<unknown> {
    return (await run(this.workspace, { path: this.path, action }, args, this.#named())).result;
  }

  async action(name: string, args: Record<string, unknown> = {}): Promise<unknown> {
    return this.#run(kebab(name), args);
  }

  /**
   * Run `action` for each of `items`, a few calls at most. Each call applies
   * up to 500 items (and about 4 MiB of JSON) as one all-or-nothing step, so
   * if any one of them refuses none of that call applies. Every call is the
   * same invocation group: History lists it once and one undo takes it back.
   * An empty list
   * sends nothing. Answers each call's answer.
   *
   * An item names one an earlier item of the same call makes with
   * {@link made}, e.g. `{ title: "Child", parent: made(0) }` for the second
   * item of a parent-then-child list: the server fills in the id the first
   * item got as the call applies.
   */
  async many(action: string, items: Record<string, unknown>[]): Promise<ActionAnswer[]> {
    const members = items.map((args) => ({ action, args }));
    if (members.length === 0) {
      return [];
    }
    const send = async (): Promise<ActionAnswer[]> => {
      const answers: ActionAnswer[] = [];
      for (const chunk of chunksOf(members)) {
        answers.push(
          await run(this.workspace, { path: this.path, action: "group" }, { actions: chunk }, this.#named()),
        );
      }
      return answers;
    };
    // The chunks are one invocation group, as `ws.invocation(...)` would make
    // them one; calls already inside a group join that one instead.
    if (currentGroup()?.workspace === this.workspace) {
      return send();
    }
    return this.workspace.invocation(`${action} (${members.length})`, send);
  }

  async edit(steps: Step[], value: unknown, op = "set"): Promise<unknown> {
    const [field] = steps;
    if (this.#setters && typeof field === "string") {
      // The field's own setter takes its whole value, or an edit inside it whose path starts
      // below the field.
      const args =
        steps.length === 1 && op === "set"
          ? { value }
          : { edit: { path: segments(steps).slice(1), op, value } };
      return this.#run(`set-${field.replaceAll("_", "-")}`, args);
    }
    return this.#run("field-edit", { edit: { path: segments(steps), op, value } });
  }

  async remove(...steps: Step[]): Promise<unknown> {
    return this.edit(steps, null, "remove-key");
  }

  schedule(steps: Step[], value: unknown, op = "set"): void {
    this.#pending.push(this.edit(steps, value, op));
  }

  /** Assign a field and keep the snapshot in step; `flush()` awaits it. */
  assign(steps: Step[], value: unknown, op = "set"): void {
    this.schedule(steps, value, op);
    const field = steps[0];
    if (typeof field !== "string" || this.#answer === null) {
      return;
    }
    const fields = (this.#answer.fields ??= {}) as Record<string, unknown>;
    if (steps.length === 1) {
      fields[field] = value;
      return;
    }
    let node = fields[field];
    for (const step of steps.slice(1, -1)) {
      if (node === null || typeof node !== "object") {
        return;
      }
      node = (node as Record<string, unknown>)[String(step)];
    }
    const last = steps[steps.length - 1];
    if (node !== null && typeof node === "object" && typeof last === "string") {
      if (op === "remove-key") {
        delete (node as Record<string, unknown>)[last];
      } else {
        (node as Record<string, unknown>)[last] = value;
      }
    }
  }

  watch(): AsyncIterableIterator<Event> {
    return this.workspace.events({ path: this.path });
  }

  /** What an unknown name is: an action, by its name in the app's words. */
  missing(name: string): unknown {
    // An action needs no read: the types that name it are the surface.
    if (name.length > MANY_SUFFIX.length && name.endsWith(MANY_SUFFIX)) {
      const action = kebab(name.slice(0, -MANY_SUFFIX.length));
      return (items: Record<string, unknown>[] = []) => this.many(action, items);
    }
    if (this.#actions.has(name)) {
      return (args: Record<string, unknown> = {}) => this.action(name, args);
    }
    if (this.#answer === null) {
      throw new Error(
        `${this.path} is not read yet: \`await ws.resource(path).load()\` first`,
      );
    }
    const key = this.wireKey(name);
    if (this.has(key)) {
      return wrap(this, [key], (this.#answer.fields as Record<string, unknown>)[key]);
    }
    return (args: Record<string, unknown> = {}) => this.action(name, args);
  }
}

/**
 * A resource as a typed object: fields, actions and field edits. `surface` is
 * the app's version the types were made from, which generated types pass, and
 * which every call through them sends.
 */
export function resourceProxy(
  workspace: Workspace,
  path: string,
  surface: Surface = null,
  actions: readonly string[] = [],
  setters = false,
): Resource {
  const named = typeof surface === "string" ? surface : (surface?.surface ?? null);
  const impl = new ResourceImpl(workspace, path, named === "" ? null : named, actions, setters);
  const proxy = new Proxy(impl, {
    get(target, property, receiver) {
      if (typeof property === "symbol") {
        return Reflect.get(target, property, receiver);
      }
      if (property in target || OWN.has(property)) {
        const value = Reflect.get(target, property, target);
        return typeof value === "function" ? value.bind(target) : value;
      }
      return target.missing(property);
    },
    set(target, property, value) {
      if (typeof property !== "string" || property in target || OWN.has(property)) {
        return Reflect.set(target, property, value);
      }
      target.assign([target.wireKey(property)], value);
      return true;
    },
    has(target, property) {
      if (typeof property === "string" && target.has(property)) {
        return true;
      }
      return Reflect.has(target, property);
    },
    ownKeys(target) {
      const fields = target.raw?.fields;
      const names = fields && typeof fields === "object" ? Object.keys(fields) : [];
      return [...new Set([...Reflect.ownKeys(target), ...names])];
    },
    getOwnPropertyDescriptor(target, property) {
      if (typeof property === "string" && target.has(property)) {
        return { configurable: true, enumerable: true, value: target.missing(property) };
      }
      return Reflect.getOwnPropertyDescriptor(target, property);
    },
  }) as unknown as Resource;
  impl.self = proxy;
  return proxy;
}

/** Every row under `path` (the apps without one), page after page: `listResources` with its
 * `next_cursor` followed. */
export async function* rows(
  workspace: Workspace,
  params: { path?: string; kind?: string; limit?: string } = {},
): AsyncGenerator<Row> {
  let cursor: string | undefined;
  do {
    const page = await workspace.listResources({ ...params, cursor });
    yield* page.rows;
    cursor = page.next_cursor ?? undefined;
  } while (cursor !== undefined);
}

/** What `exported` answers: every resource read whole, and the ones that did not read, with why. */
export interface Exported {
  resources: Read[];
  unread: { path: string; error: string }[];
}

/**
 * Everything in the workspace, read whole: every resource of every app, found by walking the
 * listings from its apps, each read with every page of what it lists. The files the resources
 * name (`tilo.content:` references) download with `workspace.download`. Take one before
 * deleting a customer's workspace.
 */
export async function exported(workspace: Workspace): Promise<Exported> {
  const found: Exported = { resources: [], unread: [] };
  const why = (error: unknown): string => (error instanceof Error ? error.message : String(error));
  const seen = new Set<string>();
  const walk: (string | undefined)[] = [undefined];
  while (walk.length > 0) {
    const path = walk.shift();
    try {
      for await (const row of rows(workspace, path === undefined ? {} : { path })) {
        if (seen.has(row.path)) {
          continue;
        }
        seen.add(row.path);
        if (row.reads !== false) {
          await readWhole(workspace, row.path).then(
            (read) => found.resources.push(read),
            (error: unknown) => found.unread.push({ path: row.path, error: why(error) }),
          );
        }
        if (row.has_children === true) {
          walk.push(row.path);
        }
      }
    } catch (error) {
      found.unread.push({ path: path ?? "", error: why(error) });
    }
  }
  return found;
}

/** `path` read with every page of what it lists. */
async function readWhole(workspace: Workspace, path: string): Promise<Read> {
  let page = await workspace.readResource({ path });
  const whole: Read = { ...page, children: [...(page.children ?? [])] };
  while (typeof page.cursor === "string" && page.cursor !== "") {
    const cursor = page.cursor;
    page = await workspace.readResource({ path, cursor });
    whole.children?.push(...(page.children ?? []));
    if (page.cursor === cursor) {
      break;
    }
  }
  delete whole.cursor;
  return whole;
}
