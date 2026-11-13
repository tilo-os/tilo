/**
 * A resource as a typed object: its fields, its actions, and its edits.
 *
 * A field reads from the resource's `fields`; a method call is the resource's
 * action (`readFormFields()` runs `read-form-fields`); assigning a name is a
 * field edit, through the generic `field-edit` action every resource
 * publishes. `form.formValues.surname = "Doe"` is the same edit one level
 * down.
 *
 * A resource made through generated types names its app's surface, and every
 * call it makes sends `Tilo-Surface` with it: a workspace whose app publishes
 * another surface refuses the call (`SurfaceChanged`) instead of running it.
 */

import type { Event } from "./models.ts";
import { ProtocolError } from "./errors.ts";
import type { Workspace } from "./workspace.ts";

/** One step of a field path: a field's name, a map key, or `{item: id}`. */
export type Step = string | Record<string, unknown>;

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
  readonly children: unknown[];
  readonly raw: Record<string, unknown>;
  load(): Promise<Resource>;
  refresh(): Promise<Resource>;
  flush(): Promise<void>;
  action(name: string, args?: Record<string, unknown>): Promise<unknown>;
  edit(steps: Step[], value: unknown, op?: string): Promise<unknown>;
  remove(...steps: Step[]): Promise<unknown>;
  watch(): AsyncGenerator<Event>;
  [name: string]: unknown;
}

/**
 * The surface a resource's calls name: generated types answer it as their
 * `static readonly surface`, and a caller may give the string itself.
 */
export type Surface = string | { surface?: string | null } | null;

/** `read_form_fields` or `readFormFields` as an action id: `read-form-fields`. */
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
    throw new ProtocolError("a field edit names a field");
  }
  return steps.map((step, index) => {
    if (typeof step === "string") {
      return index === 0 ? { field: step } : { key: step };
    }
    if (step && typeof step === "object") {
      return { ...step };
    }
    throw new ProtocolError(`a field path step is a name or a mapping, not ${String(step)}`);
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
  #answer: Record<string, unknown> | null = null;
  /** The proxy that fronts this implementation, which reads answer. */
  self: unknown = null;

  constructor(workspace: Workspace, path: string, surface: string | null = null) {
    this.workspace = workspace;
    this.path = path;
    this.surface = surface;
  }

  get kind(): string {
    return String(this.#answer?.kind ?? "");
  }

  get raw(): Record<string, unknown> {
    return this.#answer ?? {};
  }

  get children(): unknown[] {
    const children = this.#answer?.children;
    return Array.isArray(children) ? children : [];
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
    this.#answer = await this.workspace.readRaw(this.path);
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

  async action(name: string, args: Record<string, unknown> = {}): Promise<unknown> {
    return this.workspace.call(`${this.path}#${kebab(name)}`, args, this.surface);
  }

  async edit(steps: Step[], value: unknown, op = "set"): Promise<unknown> {
    return this.workspace.call(
      `${this.path}#field-edit`,
      { edit: { path: segments(steps), op, value } },
      this.surface,
    );
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
    let held = fields[field];
    for (const step of steps.slice(1, -1)) {
      if (held === null || typeof held !== "object") {
        return;
      }
      held = (held as Record<string, unknown>)[String(step)];
    }
    const last = steps[steps.length - 1];
    if (held !== null && typeof held === "object" && typeof last === "string") {
      if (op === "remove-key") {
        delete (held as Record<string, unknown>)[last];
      } else {
        (held as Record<string, unknown>)[last] = value;
      }
    }
  }

  watch(): AsyncGenerator<Event> {
    return this.workspace.watch(this.path);
  }

  /** What an unknown name is: an action, by its name in the app's words. */
  missing(name: string): unknown {
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
export function resourceProxy(workspace: Workspace, path: string, surface: Surface = null): Resource {
  const named = typeof surface === "string" ? surface : (surface?.surface ?? null);
  const impl = new ResourceImpl(workspace, path, named === "" ? null : named);
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
      const fields = target.raw.fields;
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
