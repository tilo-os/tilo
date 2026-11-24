/**
 * Types for Migrations (`so.tilo.migrate`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`migrate/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.migrate@e881061eb12ce5e8";

/** Where a migration's run stands: reviewed, with what the source holds waiting to be chosen; running, while the device that holds the data brings in what was chosen; stopped by a person, keeping what it brought until then; failed, its failure saying why; or complete. The device that holds the data moves it on as its run goes. One of "reviewed", "running", "stopped", "failed", "complete". */
export type RunState = "reviewed" | "running" | "stopped" | "failed" | "complete";

/** The arguments of `forget_items`. */
export interface ForgetItemsArgs {
  keep: string[];
}

/** The arguments of `unreadable`. */
export interface UnreadableArgs {
  what: string;
}

/** The arguments of `choose_keys`. */
export interface ChooseKeysArgs {
  keys: string[];
  chosen: boolean;
  only: boolean;
}

/** The arguments of `begun`. */
export interface BegunArgs {
  run: string;
  title: string;
}

/** The arguments of `landed`. */
export interface LandedArgs {
  run: string;
  keys: string[];
  kind: string;
  count: number;
  already: number;
}

/** The arguments of `ended`. */
export interface EndedArgs {
  state: RunState;
  failure: string;
}

/** The arguments of `forgot_run`. */
export interface ForgotRunArgs {
  run: string;
}

/** The arguments of `refused`. */
export interface RefusedArgs {
  why: string;
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

/** Item — `tl.so.tilo.migrate.Item`. */
export interface Item extends tilo.Resource {
  z7gblqe6: string;  // key
  fvmu6lru: string;  // kind
  ove3mkdj: string;  // title
  "7b7r6oxc": number;  // count
  kz76r36d: string;  // counts
  qubfwmfb: string;  // within
  ghq4i3f7: boolean;  // chosen
  "2ogrf3gp": string;  // note
}

/** The actions `Item` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ITEM_ACTIONS = [] as const;

/** The `tl.so.tilo.migrate.Item` kind at `path`. */
export function item(ws: tilo.Workspace, path: string): Item {
  return resourceProxy(ws, path, SURFACE, ITEM_ACTIONS) as unknown as Item;
}

/** Count — `tl.so.tilo.migrate.Count`. */
export interface Count extends tilo.Resource {
  "3cttshux": string;  // kind
  imoqqond: number;  // count
}

/** The actions `Count` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COUNT_ACTIONS = [] as const;

/** The `tl.so.tilo.migrate.Count` kind at `path`. */
export function count(ws: tilo.Workspace, path: string): Count {
  return resourceProxy(ws, path, SURFACE, COUNT_ACTIONS) as unknown as Count;
}

/** Run — `tl.so.tilo.migrate.Run`. */
export interface Run extends tilo.Resource {
  wphp5gxk: string;  // id
  e73pil4f: string;  // title
  ifezl4lz: string[];  // keys
}

/** The actions `Run` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RUN_ACTIONS = [] as const;

/** The `tl.so.tilo.migrate.Run` kind at `path`. */
export function run(ws: tilo.Workspace, path: string): Run {
  return resourceProxy(ws, path, SURFACE, RUN_ACTIONS) as unknown as Run;
}

/** Migration — `tl.so.tilo.migrate.Migration`. */
export interface Migration extends tilo.Resource {
  jy4y5q35: string;  // source
  d66kqgv4: string;  // title
  "6muhvdtw": string;  // device
  kxg2fplq: string;  // handle
  "5nrd34wr": Array<tilo.Entry<Item>>;  // items
  dwktqelj: string[];  // unreadable
  cbtpcpsr: RunState;  // state
  "4tzslsk2": string;  // run
  c6gmooon: string;  // run_title
  v6notall: string;  // failure
  "6tvxvli7": string;  // refused
  wtqlrste: Array<tilo.Entry<Count>>;  // brought
  hl5hwgex: number;  // already_there
  g44f5aep: string[];  // landed
  "45kagnwx": Array<tilo.Entry<Run>>;  // runs
  /** The review found the item `key`: it is kept as found, and one found before keeps whether a person chose it; a new one is chosen as `chosen` says. */
  review_item(args: {
    key: string;
    kind: string;
    title: string;
    count: number;
    counts: string;
    within: string;
    note: string;
    chosen: boolean;
  }): Promise<Item>;
  /** A fresh review no longer found the items whose keys are not in `keep`: they go. */
  forget_items(args: {
    keep: string[];
  }): Promise<void>;
  /** Runs `forget_items` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forget_itemsMany(items: ForgetItemsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The review could not read `what`. */
  unreadable(args: {
    what: string;
  }): Promise<void>;
  /** Runs `unreadable` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unreadableMany(items: UnreadableArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Chooses an item of `g` to be brought in, or leaves it out: the device that holds the data keeps the choice, and the migration follows it there, as `choose_keys`. */
  choose(args: {
    i: Item;
    chosen?: boolean;
  }): Promise<void>;
  /** The device chose the items whose keys are `keys`, or left them out; with `only`, every other item is left out. */
  choose_keys(args: {
    keys: string[];
    chosen: boolean;
    only: boolean;
  }): Promise<void>;
  /** Runs `choose_keys` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  choose_keysMany(items: ChooseKeysArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The device began the run `run`, or resumed it: it is running. A new run starts its counts again; a resumed one goes on counting. */
  begun(args: {
    run: string;
    title: string;
  }): Promise<void>;
  /** Runs `begun` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  begunMany(items: BegunArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The run `run` brought in the items `keys`: `count` things of `kind`, and found `already` of them in the workspace before. */
  landed(args: {
    run: string;
    keys: string[];
    kind: string;
    count: number;
    already: number;
  }): Promise<void>;
  /** Runs `landed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  landedMany(items: LandedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The run ended as `state`: stopped, failed with `failure`, or complete. */
  ended(args: {
    state: RunState;
    failure: string;
  }): Promise<void>;
  /** Runs `ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  endedMany(items: EndedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The device took out of the workspace what the run `run` brought in: its keys are no longer landed and the run is gone. Undoing the current run puts the migration back to reviewed. */
  forgot_run(args: {
    run: string;
  }): Promise<void>;
  /** Runs `forgot_run` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgot_runMany(items: ForgotRunArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The device refused a step asked of `g`, for `why`: nothing chosen to bring in, a source it cannot read, a run it cannot take back. */
  refused(args: {
    why: string;
  }): Promise<void>;
  /** Runs `refused` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  refusedMany(items: RefusedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Read `g`'s source again: what it holds now is what there is to choose from. */
  review(): Promise<void>;
  /** Bring in what is chosen of `g`'s source: the device that holds the data runs it as one run, and the migration follows it there, as `begun`, `landed` and `ended`. The action lasts as long as the run, and stopping it stops the run. */
  start(): Promise<void>;
  /** Stop `g`'s run after the item it is on: what it brought stays, and starting again resumes it. */
  stop(): Promise<void>;
  /** Take what the run `r` brought in back out of the workspace. */
  undo(args: {
    r: Run;
  }): Promise<void>;
}

/** The actions `Migration` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MIGRATION_ACTIONS = ["review_item", "forget_items", "forget_itemsMany", "unreadable", "unreadableMany", "choose", "choose_keys", "choose_keysMany", "begun", "begunMany", "landed", "landedMany", "ended", "endedMany", "forgot_run", "forgot_runMany", "refused", "refusedMany", "review", "start", "stop", "undo"] as const;

/** The `tl.so.tilo.migrate.Migration` kind at `path`. */
export function migration(ws: tilo.Workspace, path: string): Migration {
  return resourceProxy(ws, path, SURFACE, MIGRATION_ACTIONS) as unknown as Migration;
}

/** Migrations — `tl.so.tilo.migrate.Migrations`. */
export interface Migrations extends tilo.Resource {
  l7nahbny: Array<tilo.Entry<Migration>>;  // migrations
  /** The device that reviewed `source` lists it: the migration for that source on that device, made the first time, with its title and the handle the review read. What it could not read is cleared; the review's items follow with `review_item`. */
  reviewed(args: {
    source: string;
    title: string;
    device: string;
    handle: string;
  }): Promise<Migration>;
}

/** The actions `Migrations` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MIGRATIONS_ACTIONS = ["reviewed"] as const;

/** The `tl.so.tilo.migrate.Migrations` kind at `path`. */
export function migrations(ws: tilo.Workspace, path: string): Migrations {
  return resourceProxy(ws, path, SURFACE, MIGRATIONS_ACTIONS) as unknown as Migrations;
}

/** Migrations — `tl.so.tilo.migrate`. */
export interface Migrate extends tilo.Resource {
}

/** The actions `Migrate` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MIGRATE_ACTIONS = [] as const;

/** The `tl.so.tilo.migrate` kind at `path`. */
export function migrate(ws: tilo.Workspace, path: string): Migrate {
  return resourceProxy(ws, path, SURFACE, MIGRATE_ACTIONS) as unknown as Migrate;
}

