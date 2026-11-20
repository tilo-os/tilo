/**
 * Types for Firewall (`so.tilo.firewall`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Config` is a `Resource` of the app's `firewall.config`, made by its factory with the
 * resource's path (`firewall/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers an `Invocation`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one invocation group.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.firewall@79278c5e2f6f2928";

/** One of "allow", "deny". */
export type Action = "allow" | "deny";

/** One of Service, Port: an object whose case the app tags it with. */
export type Matcher = Record<string, unknown>;

/** One of "Tcp", "Udp". */
export type Proto = "Tcp" | "Udp";

export interface StoredRule {
  matcher: Matcher;
  action: Action;
  source: string;
  enabled: boolean;
}

export interface StoredZone {
  target: Target;
  interfaces: string[];
  services: string[];
  rules: Array<tilo.Entry<StoredRule>>;
  masquerade: boolean;
  deleted_rules: string[];
}

/** One of "default", "accept", "reject", "drop". */
export type Target = "default" | "accept" | "reject" | "drop";

/** One of "drop", "block", "public", "external", "dmz", "work", "home", "internal", "trusted". */
export type ZoneKind = "drop" | "block" | "public" | "external" | "dmz" | "work" | "home" | "internal" | "trusted";

/** The arguments of `start`. */
export interface StartArgs {
}

/** The arguments of `stop`. */
export interface StopArgs {
}

/** The arguments of `enter-panic-mode`. */
export interface EnterPanicModeArgs {
}

/** The arguments of `exit-panic-mode`. */
export interface ExitPanicModeArgs {
}

/** The arguments of `delete-rule`. */
export interface DeleteRuleArgs {
  zone: ZoneKind;
  rule: string;
}

/** The arguments of `import`. */
export interface ImportArgs {
  config: unknown;
}

/** Firewall — `firewall.config`. */
export interface Config extends tilo.Resource {
  readonly enabled: boolean;
  readonly panic_mode: boolean;
  default_zone: ZoneKind;
  zones: Record<string, StoredZone>;
  start(): Promise<void>;
  /** Runs `start` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  startMany(items: StartArgs[]): Promise<tilo.ActionAnswer[]>;
  stop(): Promise<void>;
  /** Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stopMany(items: StopArgs[]): Promise<tilo.ActionAnswer[]>;
  enterPanicMode(): Promise<void>;
  /** Runs `enter-panic-mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  enterPanicModeMany(items: EnterPanicModeArgs[]): Promise<tilo.ActionAnswer[]>;
  exitPanicMode(): Promise<void>;
  /** Runs `exit-panic-mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  exitPanicModeMany(items: ExitPanicModeArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteRule(args: {
    zone: ZoneKind;
    rule: string;
  }): Promise<void>;
  /** Runs `delete-rule` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteRuleMany(items: DeleteRuleArgs[]): Promise<tilo.ActionAnswer[]>;
  import(args: {
    config: unknown;
  }): Promise<void>;
  /** Runs `import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importMany(items: ImportArgs[]): Promise<tilo.ActionAnswer[]>;
  readBlocked(): Promise<void>;
}

/** The actions `Config` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONFIG_ACTIONS = ["start", "startMany", "stop", "stopMany", "enterPanicMode", "enterPanicModeMany", "exitPanicMode", "exitPanicModeMany", "deleteRule", "deleteRuleMany", "import", "importMany", "readBlocked"] as const;

/** The `firewall.config` kind at `path`. */
export function config(ws: tilo.Workspace, path: string): Config {
  return resourceProxy(ws, path, SURFACE, CONFIG_ACTIONS) as unknown as Config;
}

