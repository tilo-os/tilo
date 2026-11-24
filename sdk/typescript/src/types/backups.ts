/**
 * Types for Backups (`so.tilo.backups`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Entry` is a `Resource` of the app's `tl.so.tilo.backups.Entry`, made by its factory with the
 * resource's path (`backups/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.backups@7cec2ef76c91fd8a";

/** Whether a stored snapshot is whole, being checked or known broken. One of "complete", "verifying", "failed". */
export type Health = "complete" | "verifying" | "failed";

/** The retention class a snapshot counts in. One of "hourly", "daily", "weekly", "monthly", "kept", "unknown". */
export type Kept = "hourly" | "daily" | "weekly" | "monthly" | "kept" | "unknown";

/** Was a snapshot taken by the schedule or by hand? One of "automatic", "manual", "unknown". */
export type Taken = "automatic" | "manual" | "unknown";

/** When a source backs itself up. One of "on_demand", "hourly", "daily", "weekly". */
export type Cadence = "on_demand" | "hourly" | "daily" | "weekly";

/** What a source is, as much as its path says without naming it. One of "home", "system", "workspace", "custom". */
export type Origin = "home" | "system" | "workspace" | "custom";

/** The arguments of `keep_catalog`. */
export interface KeepCatalogArgs {
  answer: string;
}

/** The arguments of `back_up_now`. */
export interface BackUpNowArgs {
}

/** The arguments of `verify`. */
export interface VerifyArgs {
  s: Snapshot;
}

/** The arguments of `ask_restore`. */
export interface AskRestoreArgs {
  s: Snapshot;
}

/** The arguments of `begin_restore`. */
export interface BeginRestoreArgs {
}

/** The arguments of `restore_now`. */
export interface RestoreNowArgs {
  s: Snapshot;
}

/** The arguments of `stop_work`. */
export interface StopWorkArgs {
}

/** The arguments of `poll`. */
export interface PollArgs {
}

/** The arguments of `open_snapshot`. */
export interface OpenSnapshotArgs {
  s: Snapshot;
}

/** The arguments of `set_order`. */
export interface SetOrderArgs {
  newest_first: boolean;
}

/** The arguments of `show_place`. */
export interface ShowPlaceArgs {
  place: string;
}

/** The arguments of `keep_more`. */
export interface KeepMoreArgs {
  which: string;
}

/** The arguments of `keep_fewer`. */
export interface KeepFewerArgs {
  which: string;
}

/** The arguments of `dismiss`. */
export interface DismissArgs {
}

/** The arguments of `choose_source`. */
export interface ChooseSourceArgs {
}

/** The arguments of `choose_destination`. */
export interface ChooseDestinationArgs {
}

/** The arguments of `go`. */
export interface GoArgs {
  path: string;
}

/** The arguments of `go_up`. */
export interface GoUpArgs {
}

/** The arguments of `pick`. */
export interface PickArgs {
}

/** The arguments of `cancel_pick`. */
export interface CancelPickArgs {
}

/** The arguments of `read_picker`. */
export interface ReadPickerArgs {
}

/** The arguments of `start_work`. */
export interface StartWorkArgs {
  kind: string;
  ask: string;
}

/** The arguments of `close_snapshot`. */
export interface CloseSnapshotArgs {
}

/** The arguments of `allow_overwrite`. */
export interface AllowOverwriteArgs {
}

/** Entry — `tl.so.tilo.backups.Entry`. */
export interface Entry extends tilo.Resource {
  cvlieest: string;  // name
  c2m5wzf4: number;  // depth
  pfl5jyx2: number;  // size
  tzyurmps: boolean;  // folder
}

/** The actions `Entry` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ENTRY_ACTIONS = [] as const;

/** The `tl.so.tilo.backups.Entry` kind at `path`. */
export function entry(ws: tilo.Workspace, path: string): Entry {
  return resourceProxy(ws, path, SURFACE, ENTRY_ACTIONS) as unknown as Entry;
}

/** Snapshot — `tl.so.tilo.backups.Snapshot`. */
export interface Snapshot extends tilo.Resource {
  dallfwqf: string;  // label
  ngqbekfc: number;  // at
  hydekdnu: Taken;  // taken
  yffctwuv: Kept;  // kept
  "6kvlgzq4": Health;  // health
  ljb7imfg: number;  // size_bytes
  vcgdjszb: number;  // delta_bytes
  yr6zgb2x: number;  // files_changed
  cyesgrji: string;  // failure
  "5nvgiivd": Array<tilo.Entry<Entry>>;  // files
}

/** The actions `Snapshot` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SNAPSHOT_ACTIONS = [] as const;

/** The `tl.so.tilo.backups.Snapshot` kind at `path`. */
export function snapshot(ws: tilo.Workspace, path: string): Snapshot {
  return resourceProxy(ws, path, SURFACE, SNAPSHOT_ACTIONS) as unknown as Snapshot;
}

/** Policy — `tl.so.tilo.backups.Policy`. */
export interface Policy extends tilo.Resource {
  cyugw7lh: number;  // hourly
  avot6p6r: number;  // daily
  ywba2jzd: number;  // weekly
  "3miz6dsu": number;  // monthly
}

/** The actions `Policy` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const POLICY_ACTIONS = [] as const;

/** The `tl.so.tilo.backups.Policy` kind at `path`. */
export function policy(ws: tilo.Workspace, path: string): Policy {
  return resourceProxy(ws, path, SURFACE, POLICY_ACTIONS) as unknown as Policy;
}

/** Place — `tl.so.tilo.backups.Place`. */
export interface Place extends tilo.Resource {
  dho3qqg3: string;  // name
  f3pl3mol: string;  // path
}

/** The actions `Place` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLACE_ACTIONS = [] as const;

/** The `tl.so.tilo.backups.Place` kind at `path`. */
export function place(ws: tilo.Workspace, path: string): Place {
  return resourceProxy(ws, path, SURFACE, PLACE_ACTIONS) as unknown as Place;
}

/** Backups — `tl.so.tilo.backups.Backups`. */
export interface Backups extends tilo.Resource {
  p2imk5l5: Policy;  // policy
  srrmxp5i: string;  // source
  "6e5yqotr": string;  // destination
  "62wijgsn": string;  // source_name
  x6ca2mpx: Origin;  // source_origin
  gfsjtev7: Cadence;  // source_cadence
  "3wbouwis": boolean;  // source_failing
  v3i3cihq: string;  // destination_name
  kqbve5pf: number;  // used_bytes
  mdbssite: number;  // total_bytes
  kz5xznku: boolean;  // encrypted
  hlhhrwm3: boolean;  // destination_failing
  n7gmur3t: Array<tilo.Entry<Snapshot>>;  // snapshots
  ik3j63rv: string;  // place
  "6fg5mkqa": boolean;  // newest_first
  sxzlbnam: string;  // trouble
  yelwker2: string;  // failure
  "6rkekawk": string;  // notice
  bluheyea: string;  // job
  wmrb4vzi: string;  // working
  v7lhpobw: string;  // progress
  xsai3dea: string;  // result
  fihmdmmi: number;  // subject
  q6dvqz3z: string;  // picking
  vlgd4tne: string;  // picker
  "6nds5bnc": Array<tilo.Entry<Place>>;  // picks
  "5dqejapv": string;  // restore_into
  azcuku5y: boolean;  // overwrite
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  /** Keeps what the device answered about its repository, so the window and an agent read the same snapshots. */
  keep_catalog(args: {
    answer: string;
  }): Promise<void>;
  /** Runs `keep_catalog` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_catalogMany(items: KeepCatalogArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Backs the folder up now, keeping what the retention policy keeps. */
  back_up_now(): Promise<void>;
  /** Runs `back_up_now` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  back_up_nowMany(items: BackUpNowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Checks that every file of a snapshot is still what the snapshot recorded. */
  verify(args: {
    s: Snapshot;
  }): Promise<void>;
  /** Runs `verify` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  verifyMany(items: VerifyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the sheet that asks where a snapshot goes back to. */
  ask_restore(args: {
    s: Snapshot;
  }): Promise<void>;
  /** Runs `ask_restore` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  ask_restoreMany(items: AskRestoreArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts finding the folder a restore writes into, without running anything yet. */
  begin_restore(): Promise<void>;
  /** Runs `begin_restore` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  begin_restoreMany(items: BeginRestoreArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Restores the snapshot into the folder the person chose. */
  restore_now(args: {
    s: Snapshot;
  }): Promise<void>;
  /** Runs `restore_now` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  restore_nowMany(items: RestoreNowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the work under way; what it was writing goes. */
  stop_work(): Promise<void>;
  /** Runs `stop_work` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stop_workMany(items: StopWorkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the device how the work is doing, once a second while it runs. */
  poll(): Promise<void>;
  /** Runs `poll` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pollMany(items: PollArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one snapshot's sheet: what it holds and what can be done with it. */
  open_snapshot(args: {
    s: Snapshot;
  }): Promise<void>;
  /** Runs `open_snapshot` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_snapshotMany(items: OpenSnapshotArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Lists the timeline newest first, or oldest first. */
  set_order(args: {
    newest_first: boolean;
  }): Promise<void>;
  /** Runs `set_order` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_orderMany(items: SetOrderArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one of the sidebar's places. */
  show_place(args: {
    place: string;
  }): Promise<void>;
  /** Runs `show_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_placeMany(items: ShowPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps one more snapshot of a class. */
  keep_more(args: {
    which: string;
  }): Promise<void>;
  /** Runs `keep_more` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_moreMany(items: KeepMoreArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps one fewer snapshot of a class. */
  keep_fewer(args: {
    which: string;
  }): Promise<void>;
  /** Runs `keep_fewer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_fewerMany(items: KeepFewerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Dismisses the line under the list. */
  dismiss(): Promise<void>;
  /** Runs `dismiss` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismissMany(items: DismissArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the person which folder this device protects. */
  choose_source(): Promise<void>;
  /** Runs `choose_source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  choose_sourceMany(items: ChooseSourceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the person where the snapshots go. */
  choose_destination(): Promise<void>;
  /** Runs `choose_destination` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  choose_destinationMany(items: ChooseDestinationArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Walks the picker to another folder and shows the folders in it. */
  go(args: {
    path: string;
  }): Promise<void>;
  /** Runs `go` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  goMany(items: GoArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Walks the picker up one folder. */
  go_up(): Promise<void>;
  /** Runs `go_up` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  go_upMany(items: GoUpArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the folder the picker shows: as what this device protects, where snapshots go, or where a restore writes. */
  pick(): Promise<void>;
  /** Runs `pick` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pickMany(items: PickArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Leaves the picker without changing anything. */
  cancel_pick(): Promise<void>;
  /** Runs `cancel_pick` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancel_pickMany(items: CancelPickArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the folders of the one the picker shows. */
  read_picker(): Promise<void>;
  /** Runs `read_picker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_pickerMany(items: ReadPickerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a piece of work and keeps the job the device gave it. */
  start_work(args: {
    kind: string;
    ask: string;
  }): Promise<void>;
  /** Runs `start_work` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  start_workMany(items: StartWorkArgs[]): Promise<tilo.ActionAnswer[]>;
  close_snapshot(): Promise<void>;
  /** Runs `close_snapshot` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  close_snapshotMany(items: CloseSnapshotArgs[]): Promise<tilo.ActionAnswer[]>;
  allow_overwrite(): Promise<void>;
  /** Runs `allow_overwrite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  allow_overwriteMany(items: AllowOverwriteArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Backups` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BACKUPS_ACTIONS = ["keep_catalog", "keep_catalogMany", "back_up_now", "back_up_nowMany", "verify", "verifyMany", "ask_restore", "ask_restoreMany", "begin_restore", "begin_restoreMany", "restore_now", "restore_nowMany", "stop_work", "stop_workMany", "poll", "pollMany", "open_snapshot", "open_snapshotMany", "set_order", "set_orderMany", "show_place", "show_placeMany", "keep_more", "keep_moreMany", "keep_fewer", "keep_fewerMany", "dismiss", "dismissMany", "choose_source", "choose_sourceMany", "choose_destination", "choose_destinationMany", "go", "goMany", "go_up", "go_upMany", "pick", "pickMany", "cancel_pick", "cancel_pickMany", "read_picker", "read_pickerMany", "start_work", "start_workMany", "close_snapshot", "close_snapshotMany", "allow_overwrite", "allow_overwriteMany"] as const;

/** The `tl.so.tilo.backups.Backups` kind at `path`. */
export function backups(ws: tilo.Workspace, path: string): Backups {
  return resourceProxy(ws, path, SURFACE, BACKUPS_ACTIONS) as unknown as Backups;
}

/** CatalogAsk — `tl.so.tilo.backups.CatalogAsk`. */
export interface CatalogAsk extends tilo.Resource {
  gbluxeo3: string;  // source
  ixnr6cnx: string;  // destination
}

/** The actions `CatalogAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CATALOGASK_ACTIONS = [] as const;

/** The `tl.so.tilo.backups.CatalogAsk` kind at `path`. */
export function catalogAsk(ws: tilo.Workspace, path: string): CatalogAsk {
  return resourceProxy(ws, path, SURFACE, CATALOGASK_ACTIONS) as unknown as CatalogAsk;
}

/** BackupAsk — `tl.so.tilo.backups.BackupAsk`. */
export interface BackupAsk extends tilo.Resource {
  w7elqfbk: string;  // source
  ebybhxkw: string;  // destination
  qqrrtmrs: number;  // policy_hourly
  qrbyjmwo: number;  // policy_daily
  jd3hqbyz: number;  // policy_weekly
  "6z7komoa": number;  // policy_monthly
}

/** The actions `BackupAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BACKUPASK_ACTIONS = [] as const;

/** The `tl.so.tilo.backups.BackupAsk` kind at `path`. */
export function backupAsk(ws: tilo.Workspace, path: string): BackupAsk {
  return resourceProxy(ws, path, SURFACE, BACKUPASK_ACTIONS) as unknown as BackupAsk;
}

/** SnapshotAsk — `tl.so.tilo.backups.SnapshotAsk`. */
export interface SnapshotAsk extends tilo.Resource {
  "2stygd3d": string;  // destination
  vfb2hw7j: string;  // snapshot
}

/** The actions `SnapshotAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SNAPSHOTASK_ACTIONS = [] as const;

/** The `tl.so.tilo.backups.SnapshotAsk` kind at `path`. */
export function snapshotAsk(ws: tilo.Workspace, path: string): SnapshotAsk {
  return resourceProxy(ws, path, SURFACE, SNAPSHOTASK_ACTIONS) as unknown as SnapshotAsk;
}

/** RestoreAsk — `tl.so.tilo.backups.RestoreAsk`. */
export interface RestoreAsk extends tilo.Resource {
  z2pcuhft: string;  // destination
  qz4acm4a: string;  // snapshot
  "3hxnk3mo": string;  // target
  lbwhdcbx: boolean;  // overwrite
}

/** The actions `RestoreAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RESTOREASK_ACTIONS = [] as const;

/** The `tl.so.tilo.backups.RestoreAsk` kind at `path`. */
export function restoreAsk(ws: tilo.Workspace, path: string): RestoreAsk {
  return resourceProxy(ws, path, SURFACE, RESTOREASK_ACTIONS) as unknown as RestoreAsk;
}

/** WorkState — `tl.so.tilo.backups.WorkState`. */
export interface WorkState extends tilo.Resource {
  ptww3noe: string;  // state
  pnevrulc: string;  // progress
  lrfiygoo: string;  // answer
  apwu74vg: string;  // error
}

/** The actions `WorkState` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKSTATE_ACTIONS = [] as const;

/** The `tl.so.tilo.backups.WorkState` kind at `path`. */
export function workState(ws: tilo.Workspace, path: string): WorkState {
  return resourceProxy(ws, path, SURFACE, WORKSTATE_ACTIONS) as unknown as WorkState;
}

/** Checked — `tl.so.tilo.backups.Checked`. */
export interface Checked extends tilo.Resource {
  lg6ud7lc: number;  // files
  deudvrko: number;  // bytes
}

/** The actions `Checked` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHECKED_ACTIONS = [] as const;

/** The `tl.so.tilo.backups.Checked` kind at `path`. */
export function checked(ws: tilo.Workspace, path: string): Checked {
  return resourceProxy(ws, path, SURFACE, CHECKED_ACTIONS) as unknown as Checked;
}

/** Backups — `tl.so.tilo.backups`. */
export interface TiloBackups extends tilo.Resource {
}

/** The actions `TiloBackups` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOBACKUPS_ACTIONS = [] as const;

/** The `tl.so.tilo.backups` kind at `path`. */
export function tiloBackups(ws: tilo.Workspace, path: string): TiloBackups {
  return resourceProxy(ws, path, SURFACE, TILOBACKUPS_ACTIONS) as unknown as TiloBackups;
}

