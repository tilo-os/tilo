/**
 * Types for App Publisher (`so.tilo.publisher`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`publisher/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.publisher@61f62c65e169cd7f";

/** The arguments of `read_key`. */
export interface ReadKeyArgs {
}

/** The arguments of `create_key`. */
export interface CreateKeyArgs {
}

/** The arguments of `apply_for_namespaces`. */
export interface ApplyForNamespacesArgs {
}

/** The arguments of `check_status`. */
export interface CheckStatusArgs {
}

/** The arguments of `answered`. */
export interface AnsweredArgs {
  j: unknown;
}

/** Notice — `tl.tilo.host.Notice`. */
export interface Notice extends tilo.Resource {
  notice_to: string;  // to
  notice_title: string;  // title
  notice_body: string;  // body
  notice_opens: unknown;  // opens
  notice_seen: boolean;  // seen
  notice_device: string;  // device
  notice_actions: string[];  // actions
}

/** The actions `Notice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTICE_ACTIONS = [] as const;

/** The `tl.tilo.host.Notice` kind at `path`. */
export function notice(ws: tilo.Workspace, path: string): Notice {
  return resourceProxy(ws, path, SURFACE, NOTICE_ACTIONS) as unknown as Notice;
}

/** Reading — `tl.so.tilo.publisher.Reading`. */
export interface Reading extends tilo.Resource {
  hia3wxhi: string;  // at
  ntbsg676: string;  // summary
}

/** The actions `Reading` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const READING_ACTIONS = [] as const;

/** The `tl.so.tilo.publisher.Reading` kind at `path`. */
export function reading(ws: tilo.Workspace, path: string): Reading {
  return resourceProxy(ws, path, SURFACE, READING_ACTIONS) as unknown as Reading;
}

/** Publisher — `tl.so.tilo.publisher.Publisher`. */
export interface Publisher extends tilo.Resource {
  izqbnlov: string;  // registry
  "4hwurbpa": string;  // domain
  "6fcxou37": string;  // forge_account
  "22elkpx4": string;  // contact
  rgwkewz7: string;  // key
  b63dald4: string;  // public_key
  b6h3el5f: string;  // notice_kind
  laxvdhhk: string;  // notice_title
  ara6pea6: string;  // notice_detail
  vu53jaba: string;  // job
  etzmfdag: Array<tilo.Entry<Reading>>;  // readings
  /** Reads this computer's publisher key: whether it has one, and its public half. Opening the app reads it, so the window shows the key's state without anything to press. */
  read_key(): Promise<void>;
  /** Runs `read_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_keyMany(items: ReadKeyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes this computer's publisher key. It signs the person's apps and their requests to the registry, and a second one is never made: a publisher key is one per machine. */
  create_key(): Promise<void>;
  /** Runs `create_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  create_keyMany(items: CreateKeyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies the proof line: the words to publish where only the person can write. */
  copy_proof(): Promise<void>;
  /** Copies the key file, for the person's CI: one secret, `TILO_PUBLISHER_KEY`, holding it. */
  copy_secret(): Promise<void>;
  /** The key file, for the person to keep somewhere safe: what they restore the key from. */
  export_key(): Promise<unknown>;
  /** Asks the registry for the namespaces the profile's domain and forge account name. */
  apply_for_namespaces(): Promise<void>;
  /** Runs `apply_for_namespaces` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  apply_for_namespacesMany(items: ApplyForNamespacesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads what the registry holds about this computer's key's application. */
  check_status(): Promise<void>;
  /** Runs `check_status` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  check_statusMany(items: CheckStatusArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The registry answered: what it said about the application, or why it could not. */
  answered(args: {
    j: unknown;
  }): Promise<void>;
  /** Runs `answered` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  answeredMany(items: AnsweredArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Publisher` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PUBLISHER_ACTIONS = ["read_key", "read_keyMany", "create_key", "create_keyMany", "copy_proof", "copy_secret", "export_key", "apply_for_namespaces", "apply_for_namespacesMany", "check_status", "check_statusMany", "answered", "answeredMany"] as const;

/** The `tl.so.tilo.publisher.Publisher` kind at `path`. */
export function publisher(ws: tilo.Workspace, path: string): Publisher {
  return resourceProxy(ws, path, SURFACE, PUBLISHER_ACTIONS) as unknown as Publisher;
}

/** ForgeParts — `tl.so.tilo.publisher.ForgeParts`. */
export interface ForgeParts extends tilo.Resource {
  y36hrf57: string;  // host
  iwdc7ejj: string;  // account
}

/** The actions `ForgeParts` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FORGEPARTS_ACTIONS = [] as const;

/** The `tl.so.tilo.publisher.ForgeParts` kind at `path`. */
export function forgeParts(ws: tilo.Workspace, path: string): ForgeParts {
  return resourceProxy(ws, path, SURFACE, FORGEPARTS_ACTIONS) as unknown as ForgeParts;
}

/** KeyFile — `tl.so.tilo.publisher.KeyFile`. */
export interface KeyFile extends tilo.Resource {
  "6347lg4i": string;  // text
}

/** The actions `KeyFile` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const KEYFILE_ACTIONS = [] as const;

/** The `tl.so.tilo.publisher.KeyFile` kind at `path`. */
export function keyFile(ws: tilo.Workspace, path: string): KeyFile {
  return resourceProxy(ws, path, SURFACE, KEYFILE_ACTIONS) as unknown as KeyFile;
}

/** ApplyAsk — `tl.so.tilo.publisher.ApplyAsk`. */
export interface ApplyAsk extends tilo.Resource {
  gh566qcn: string;  // registry
  eb4v6asr: string;  // domain
  hjj2vyhq: string;  // forge_account
  e2wp4tw7: string;  // contact
}

/** The actions `ApplyAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const APPLYASK_ACTIONS = [] as const;

/** The `tl.so.tilo.publisher.ApplyAsk` kind at `path`. */
export function applyAsk(ws: tilo.Workspace, path: string): ApplyAsk {
  return resourceProxy(ws, path, SURFACE, APPLYASK_ACTIONS) as unknown as ApplyAsk;
}

/** StatusAsk — `tl.so.tilo.publisher.StatusAsk`. */
export interface StatusAsk extends tilo.Resource {
  lbble236: string;  // registry
}

/** The actions `StatusAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STATUSASK_ACTIONS = [] as const;

/** The `tl.so.tilo.publisher.StatusAsk` kind at `path`. */
export function statusAsk(ws: tilo.Workspace, path: string): StatusAsk {
  return resourceProxy(ws, path, SURFACE, STATUSASK_ACTIONS) as unknown as StatusAsk;
}

/** RegistryWords — `tl.so.tilo.publisher.RegistryWords`. */
export interface RegistryWords extends tilo.Resource {
  "2gablpyd": string;  // summary
}

/** The actions `RegistryWords` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REGISTRYWORDS_ACTIONS = [] as const;

/** The `tl.so.tilo.publisher.RegistryWords` kind at `path`. */
export function registryWords(ws: tilo.Workspace, path: string): RegistryWords {
  return resourceProxy(ws, path, SURFACE, REGISTRYWORDS_ACTIONS) as unknown as RegistryWords;
}

/** KeyState — `tl.so.tilo.publisher.KeyState`. */
export interface KeyState extends tilo.Resource {
  iyvbvvra: string;  // state
  "5gktvyxf": string;  // public
}

/** The actions `KeyState` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const KEYSTATE_ACTIONS = [] as const;

/** The `tl.so.tilo.publisher.KeyState` kind at `path`. */
export function keyState(ws: tilo.Workspace, path: string): KeyState {
  return resourceProxy(ws, path, SURFACE, KEYSTATE_ACTIONS) as unknown as KeyState;
}

/** App Publisher — `tl.so.tilo.publisher`. */
export interface TiloPublisher extends tilo.Resource {
}

/** The actions `TiloPublisher` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOPUBLISHER_ACTIONS = [] as const;

/** The `tl.so.tilo.publisher` kind at `path`. */
export function tiloPublisher(ws: tilo.Workspace, path: string): TiloPublisher {
  return resourceProxy(ws, path, SURFACE, TILOPUBLISHER_ACTIONS) as unknown as TiloPublisher;
}

