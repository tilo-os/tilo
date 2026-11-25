/**
 * Types for Disk Usage Analyzer (`so.tilo.disk-usage`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`disk-usage/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.disk-usage@8d7a57b84ae32aae";

/** The arguments of `analyze`. */
export interface AnalyzeArgs {
  path: string;
  label: string;
}

/** The arguments of `drill`. */
export interface DrillArgs {
  m: Measure;
}

/** The arguments of `open_place`. */
export interface OpenPlaceArgs {
  path: string;
}

/** The arguments of `back`. */
export interface BackArgs {
}

/** The arguments of `home`. */
export interface HomeArgs {
}

/** The arguments of `choose`. */
export interface ChooseArgs {
}

/** The arguments of `go`. */
export interface GoArgs {
  path: string;
}

/** The arguments of `pick`. */
export interface PickArgs {
}

/** The arguments of `cancel_choose`. */
export interface CancelChooseArgs {
}

/** The arguments of `read_picker`. */
export interface ReadPickerArgs {
}

/** The arguments of `poll`. */
export interface PollArgs {
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

/** Measure — `tl.so.tilo.disk-usage.Measure`. */
export interface Measure extends tilo.Resource {
  dgpcdvfo: string;  // name
  hr7pdtyh: string;  // path
  "5ejkceq4": number;  // bytes
  h6ycnyru: number;  // files
}

/** The actions `Measure` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MEASURE_ACTIONS = [] as const;

/** The `tl.so.tilo.disk-usage.Measure` kind at `path`. */
export function measure(ws: tilo.Workspace, path: string): Measure {
  return resourceProxy(ws, path, SURFACE, MEASURE_ACTIONS) as unknown as Measure;
}

/** Recent — `tl.so.tilo.disk-usage.Recent`. */
export interface Recent extends tilo.Resource {
  mks7bf3q: string;  // path
  w6urofdz: string;  // label
  rrwtdebb: string;  // seen
}

/** The actions `Recent` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RECENT_ACTIONS = [] as const;

/** The `tl.so.tilo.disk-usage.Recent` kind at `path`. */
export function recent(ws: tilo.Workspace, path: string): Recent {
  return resourceProxy(ws, path, SURFACE, RECENT_ACTIONS) as unknown as Recent;
}

/** Analyzer — `tl.so.tilo.disk-usage.Analyzer`. */
export interface Analyzer extends tilo.Resource {
  myw6qh3s: string;  // folder
  thlese6a: string;  // label
  w37l5txv: Array<tilo.Entry<Measure>>;  // entries
  of6rtqrq: number;  // total
  h7sbqkay: number;  // files
  qgfvwvqs: number;  // unreadable
  ciznalbq: string;  // trouble
  qirhun4g: string;  // notice
  xpqqxnzx: string;  // job
  bborqgwr: number;  // seen_bytes
  jycgqaau: number;  // seen_files
  tby3fdb6: string;  // root
  neoxlbvt: Array<tilo.Entry<Recent>>;  // recents
  jwzq6fuq: boolean;  // choosing
  eynuk6dc: string;  // picker
  o5hekdfu: Array<tilo.Entry<Measure>>;  // picks
  /** The folder the person chose, made the folder measured now and its own chain's start. */
  analyze(args: {
    path: string;
    label: string;
  }): Promise<void>;
  /** Runs `analyze` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  analyzeMany(items: AnalyzeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Measures a folder inside the one measured, keeping the way back. */
  drill(args: {
    m: Measure;
  }): Promise<void>;
  /** Runs `drill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  drillMany(items: DrillArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Measures a folder offered on the places page, starting a chain of its own. */
  open_place(args: {
    path: string;
  }): Promise<void>;
  /** Runs `open_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_placeMany(items: OpenPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the measuring under way. */
  stop(): Promise<void>;
  /** Goes back one folder, and from the folder the chain started at to the places page. */
  back(): Promise<void>;
  /** Runs `back` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  backMany(items: BackArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the places, on this device and measured lately. */
  home(): Promise<void>;
  /** Runs `home` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  homeMany(items: HomeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the person which folder to measure, showing the folders of this device. */
  choose(): Promise<void>;
  /** Runs `choose` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  chooseMany(items: ChooseArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Walks the picker to `path` and shows the folders in it. */
  go(args: {
    path: string;
  }): Promise<void>;
  /** Runs `go` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  goMany(items: GoArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Measures the folder the picker shows. */
  pick(): Promise<void>;
  /** Runs `pick` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pickMany(items: PickArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Leaves the chooser without measuring anything. */
  cancel_choose(): Promise<void>;
  /** Runs `cancel_choose` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancel_chooseMany(items: CancelChooseArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the folders of the one the picker shows. */
  read_picker(): Promise<void>;
  /** Runs `read_picker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_pickerMany(items: ReadPickerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks how far the measuring has got: the window's own clock, once a second while it runs. */
  poll(): Promise<void>;
  /** Runs `poll` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pollMany(items: PollArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Analyzer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ANALYZER_ACTIONS = ["analyze", "analyzeMany", "drill", "drillMany", "open_place", "open_placeMany", "stop", "back", "backMany", "home", "homeMany", "choose", "chooseMany", "go", "goMany", "pick", "pickMany", "cancel_choose", "cancel_chooseMany", "read_picker", "read_pickerMany", "poll", "pollMany"] as const;

/** The `tl.so.tilo.disk-usage.Analyzer` kind at `path`. */
export function analyzer(ws: tilo.Workspace, path: string): Analyzer {
  return resourceProxy(ws, path, SURFACE, ANALYZER_ACTIONS) as unknown as Analyzer;
}

/** ScanAsk — `tl.so.tilo.disk-usage.ScanAsk`. */
export interface ScanAsk extends tilo.Resource {
  fzzlfxy7: string;  // path
}

/** The actions `ScanAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCANASK_ACTIONS = [] as const;

/** The `tl.so.tilo.disk-usage.ScanAsk` kind at `path`. */
export function scanAsk(ws: tilo.Workspace, path: string): ScanAsk {
  return resourceProxy(ws, path, SURFACE, SCANASK_ACTIONS) as unknown as ScanAsk;
}

/** JobAnswer — `tl.so.tilo.disk-usage.JobAnswer`. */
export interface JobAnswer extends tilo.Resource {
  "42vybryr": string;  // job
}

/** The actions `JobAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const JOBANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.disk-usage.JobAnswer` kind at `path`. */
export function jobAnswer(ws: tilo.Workspace, path: string): JobAnswer {
  return resourceProxy(ws, path, SURFACE, JOBANSWER_ACTIONS) as unknown as JobAnswer;
}

/** Scanned — `tl.so.tilo.disk-usage.Scanned`. */
export interface Scanned extends tilo.Resource {
  "5asfkn6k": string;  // name
  "7xc3ifdt": string;  // path
  elhtrr33: number;  // bytes
  ktl6erww: number;  // files
}

/** The actions `Scanned` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCANNED_ACTIONS = [] as const;

/** The `tl.so.tilo.disk-usage.Scanned` kind at `path`. */
export function scanned(ws: tilo.Workspace, path: string): Scanned {
  return resourceProxy(ws, path, SURFACE, SCANNED_ACTIONS) as unknown as Scanned;
}

/** ScanState — `tl.so.tilo.disk-usage.ScanState`. */
export interface ScanState extends tilo.Resource {
  qaev2xl3: string;  // state
  dmlnkvla: string;  // path
  k2iebb3k: number;  // bytes
  zzb64yrs: number;  // files
  "7gwl2int": number;  // total
  uvlehuak: number;  // unreadable
  f7hqirco: string;  // error
}

/** The actions `ScanState` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCANSTATE_ACTIONS = [] as const;

/** The `tl.so.tilo.disk-usage.ScanState` kind at `path`. */
export function scanState(ws: tilo.Workspace, path: string): ScanState {
  return resourceProxy(ws, path, SURFACE, SCANSTATE_ACTIONS) as unknown as ScanState;
}

/** Disk Usage Analyzer — `tl.so.tilo.disk-usage`. */
export interface DiskUsage extends tilo.Resource {
  /** Opens the Disks app, where the device's drives and their use are. */
  open_disks(): Promise<void>;
}

/** The actions `DiskUsage` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DISKUSAGE_ACTIONS = ["open_disks"] as const;

/** The `tl.so.tilo.disk-usage` kind at `path`. */
export function diskUsage(ws: tilo.Workspace, path: string): DiskUsage {
  return resourceProxy(ws, path, SURFACE, DISKUSAGE_ACTIONS) as unknown as DiskUsage;
}

