/**
 * Types for Diff & Merge (`so.tilo.diffmerge`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`diffmerge/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.diffmerge@fbad1251ec66b978";

/** The arguments of `take_ours`. */
export interface TakeOursArgs {
  hunk: number;
}

/** The arguments of `take_theirs`. */
export interface TakeTheirsArgs {
  hunk: number;
}

/** The arguments of `take_both`. */
export interface TakeBothArgs {
  hunk: number;
}

/** The arguments of `resolve`. */
export interface ResolveArgs {
  hunk: number;
  text: string;
}

/** The arguments of `compare_folders`. */
export interface CompareFoldersArgs {
  left?: string;
  right?: string;
}

/** The arguments of `chosen_folder_left`. */
export interface ChosenFolderLeftArgs {
  folder: string;
}

/** The arguments of `chosen_folder_right`. */
export interface ChosenFolderRightArgs {
  folder: string;
}

/** The arguments of `edit_merged`. */
export interface EditMergedArgs {
  text: string;
}

/** The arguments of `save_merged`. */
export interface SaveMergedArgs {
  path: string;
}

/** The arguments of `fill_sample`. */
export interface FillSampleArgs {
}

/** The arguments of `sample_folders`. */
export interface SampleFoldersArgs {
}

/** The arguments of `clear_notice`. */
export interface ClearNoticeArgs {
}

/** The arguments of `forget`. */
export interface ForgetArgs {
  s: Session;
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

/** Block — `tl.so.tilo.diffmerge.Block`. */
export interface Block extends tilo.Resource {
  csjrkfwp: number;  // first
  cjhoemux: number;  // last
  w2shjabm: string;  // kind
  vwniktmt: string;  // preview
}

/** The actions `Block` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BLOCK_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.Block` kind at `path`. */
export function block(ws: tilo.Workspace, path: string): Block {
  return resourceProxy(ws, path, SURFACE, BLOCK_ACTIONS) as unknown as Block;
}

/** Hunk — `tl.so.tilo.diffmerge.Hunk`. */
export interface Hunk extends tilo.Resource {
  "47hfb7ea": number;  // index
  "4qm5ujaf": string;  // resolution
  m2r6lioa: string;  // base
  msvfenbm: string;  // ours
  ojqeurrr: string;  // theirs
}

/** The actions `Hunk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HUNK_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.Hunk` kind at `path`. */
export function hunk(ws: tilo.Workspace, path: string): Hunk {
  return resourceProxy(ws, path, SURFACE, HUNK_ACTIONS) as unknown as Hunk;
}

/** FolderEntry — `tl.so.tilo.diffmerge.FolderEntry`. */
export interface FolderEntry extends tilo.Resource {
  usttvqtv: string;  // path
  q6kpasi4: string;  // kind
  u5jul62r: number;  // left_len
  "4in5gnvl": number;  // right_len
  dcnmuoj3: boolean;  // by_length
}

/** The actions `FolderEntry` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FOLDERENTRY_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.FolderEntry` kind at `path`. */
export function folderEntry(ws: tilo.Workspace, path: string): FolderEntry {
  return resourceProxy(ws, path, SURFACE, FOLDERENTRY_ACTIONS) as unknown as FolderEntry;
}

/** Session — `tl.so.tilo.diffmerge.Session`. */
export interface Session extends tilo.Resource {
  j6uarraa: string;  // name
  odsfatc7: string;  // left_label
  kcdcrgzi: string;  // left
  "3q7df7eg": string;  // right_label
  prwrxrg4: string;  // right
  ebpdel6u: string;  // left_aligned
  vjlsurzf: string;  // right_aligned
  "7obphfng": string;  // kinds
  ogpmdy55: Array<tilo.Entry<Block>>;  // blocks
  ie5v4a4i: number;  // added
  hv4ba7bq: number;  // removed
  "6a2srqu5": number;  // modified
  "2f7twk55": boolean;  // identical
  "34arfoar": boolean;  // compared
  mhtp452w: string;  // base_label
  "5ozczfsi": string;  // base
  czecclw6: string;  // ours_label
  "75vbkvt3": string;  // ours
  "6iwqekxm": string;  // theirs_label
  zm3cqlts: string;  // theirs
  c7es3yev: string;  // merged
  twukcquq: string;  // composed
  gvqybypj: Array<tilo.Entry<Hunk>>;  // hunks
  bolkamra: number;  // changes
  mjnnfnbm: number;  // conflicts
  n7puwfba: boolean;  // merging
  yif52d5b: string;  // folder_choice_left
  cr3zcxiz: string;  // folder_choice_right
  w5y6trs7: string;  // folder_left_label
  "7i3hhhy2": string;  // folder_right_label
  mbawdpwr: string;  // folder_left_root
  utsx5nqp: string;  // folder_right_root
  tn6u7t4i: Array<tilo.Entry<FolderEntry>>;  // folder_entries
  "7ceymc4l": boolean;  // folder_truncated
  z4cg3ljy: boolean;  // folding
  toc5vveo: string;  // saved_path
  avjlapbd: string;  // saved_name
  nkku72cz: string;  // saved
  ttqrgoca: string;  // notice
  dr64yk6f: string;  // notice_kind
  /** Compares two texts: what a script, an agent or another app asks of Diff & Merge. */
  open_comparison(args: {
    left_label: string;
    left: string;
    right_label: string;
    right: string;
  }): Promise<Session>;
  /** The action the person's file chooser fills for the left side: the left file, and then the right one. */
  chosen_left(args: {
    file: unknown;
  }): Promise<Session>;
  /** The action the person's file chooser fills for the right side. */
  chosen_right(args: {
    file: unknown;
  }): Promise<Session>;
  /** Asks the device to align the two sides, and holds what it made: the aligned columns, the rows that changed and how many of each. */
  refresh_comparison(): Promise<boolean>;
  /** Merges three texts: a common base and the two versions of it a person has. */
  open_merge(args: {
    base_label: string;
    base: string;
    ours_label: string;
    ours: string;
    theirs_label: string;
    theirs: string;
  }): Promise<Session>;
  /** Asks the device to hunk the merge and compose the result with what the person took so far. */
  refresh_merge(): Promise<boolean>;
  /** Takes our side for a change. */
  take_ours(args: {
    hunk: number;
  }): Promise<void>;
  /** Runs `take_ours` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  take_oursMany(items: TakeOursArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes their side for a change. */
  take_theirs(args: {
    hunk: number;
  }): Promise<void>;
  /** Runs `take_theirs` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  take_theirsMany(items: TakeTheirsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes both sides for a change, ours first. */
  take_both(args: {
    hunk: number;
  }): Promise<void>;
  /** Runs `take_both` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  take_bothMany(items: TakeBothArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Resolves a change with the person's own text. */
  resolve(args: {
    hunk: number;
    text: string;
  }): Promise<void>;
  /** Runs `resolve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resolveMany(items: ResolveArgs[]): Promise<tilo.ActionAnswer[]>;
  take(args: {
    hunk: number;
    resolution: string;
  }): Promise<boolean>;
  compose_into(): Promise<boolean>;
  /** Compares the two folders at `left` and `right` on this device: what only one holds, and what both hold but differ in. Two absolute paths, or two empty ones to choose the folders at the window. */
  compare_folders(args?: {
    left?: string;
    right?: string;
  }): Promise<void>;
  /** Runs `compare_folders` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  compare_foldersMany(items: CompareFoldersArgs[]): Promise<tilo.ActionAnswer[]>;
  hold_folders(args: {
    answer: string;
  }): Promise<boolean>;
  /** The action the person's folder chooser fills for the left tree. */
  chosen_folder_left(args: {
    folder: string;
  }): Promise<void>;
  /** Runs `chosen_folder_left` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  chosen_folder_leftMany(items: ChosenFolderLeftArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The action the person's folder chooser fills for the right tree: both chosen, the folders are compared. */
  chosen_folder_right(args: {
    folder: string;
  }): Promise<void>;
  /** Runs `chosen_folder_right` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  chosen_folder_rightMany(items: ChosenFolderRightArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens one file of the folder comparison: both its sides when both trees hold it, else the one that does, against nothing. */
  open_folder_entry(args: {
    entry: FolderEntry;
  }): Promise<Session>;
  /** Opens the file of the row the person pressed, for the key that asked: Enter opens it. */
  open_on_key(args: {
    entry: FolderEntry;
    key: string;
  }): Promise<Session>;
  /** Replaces the merged result: what a person types into it, and what an agent or a script writes when the result is theirs to change. Once it is not what the sides compose, taking a side would throw the edit away, so the rest is resolved in the text. */
  edit_merged(args: {
    text: string;
  }): Promise<void>;
  /** Runs `edit_merged` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  edit_mergedMany(items: EditMergedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes the merged result to `path` on this device. */
  save_merged(args: {
    path: string;
  }): Promise<void>;
  /** Runs `save_merged` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  save_mergedMany(items: SaveMergedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The merged result as a file the person places with the system's save chooser, which the window shows. */
  save_bytes(): Promise<unknown>;
  /** Fills a comparison with the samples: the two-way comparison and the three-way merge, so a person sees how each reads before they have two files at hand. */
  fill_sample(): Promise<void>;
  /** Runs `fill_sample` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  fill_sampleMany(items: FillSampleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the sample folder comparison: no disk is read for it. */
  sample_folders(): Promise<void>;
  /** Runs `sample_folders` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sample_foldersMany(items: SampleFoldersArgs[]): Promise<tilo.ActionAnswer[]>;
  refuse(args: {
    why: string;
  }): Promise<boolean>;
  clear_notice(): Promise<void>;
  /** Runs `clear_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_noticeMany(items: ClearNoticeArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Session` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SESSION_ACTIONS = ["open_comparison", "chosen_left", "chosen_right", "refresh_comparison", "open_merge", "refresh_merge", "take_ours", "take_oursMany", "take_theirs", "take_theirsMany", "take_both", "take_bothMany", "resolve", "resolveMany", "take", "compose_into", "compare_folders", "compare_foldersMany", "hold_folders", "chosen_folder_left", "chosen_folder_leftMany", "chosen_folder_right", "chosen_folder_rightMany", "open_folder_entry", "open_on_key", "edit_merged", "edit_mergedMany", "save_merged", "save_mergedMany", "save_bytes", "fill_sample", "fill_sampleMany", "sample_folders", "sample_foldersMany", "refuse", "clear_notice", "clear_noticeMany"] as const;

/** The `tl.so.tilo.diffmerge.Session` kind at `path`. */
export function session(ws: tilo.Workspace, path: string): Session {
  return resourceProxy(ws, path, SURFACE, SESSION_ACTIONS) as unknown as Session;
}

/** Library — `tl.so.tilo.diffmerge.Library`. */
export interface Library extends tilo.Resource {
  tzmh25hr: Array<tilo.Entry<Session>>;  // sessions
  /** Starts an empty comparison: a place to open two files in. */
  new_comparison(): Promise<Session>;
  /** Opens the two files at `left` and `right` on this device and compares them. Two absolute paths, or two empty ones to choose the files at the window. */
  open_files(args?: {
    left?: string;
    right?: string;
  }): Promise<Session>;
  /** Opens one file on this device against nothing: what a folder comparison shows for a file only one tree holds. `side` is `left` or `right`. */
  open_one(args: {
    path: string;
    side?: string;
  }): Promise<Session>;
  /** Opens the sample comparison and the sample merge, so a person sees how each reads before they have two files at hand. */
  load_sample(): Promise<Session>;
  forget(args: {
    s: Session;
  }): Promise<void>;
  /** Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetMany(items: ForgetArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["new_comparison", "open_files", "open_one", "load_sample", "forget", "forgetMany"] as const;

/** The `tl.so.tilo.diffmerge.Library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** OpenAsk — `tl.so.tilo.diffmerge.OpenAsk`. */
export interface OpenAsk extends tilo.Resource {
  ym5ajbxk: string;  // path
  ou5hf2bx: string;  // address
  qqwavfqy: string;  // name
}

/** The actions `OpenAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OPENASK_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.OpenAsk` kind at `path`. */
export function openAsk(ws: tilo.Workspace, path: string): OpenAsk {
  return resourceProxy(ws, path, SURFACE, OPENASK_ACTIONS) as unknown as OpenAsk;
}

/** OpenedAnswer — `tl.so.tilo.diffmerge.OpenedAnswer`. */
export interface OpenedAnswer extends tilo.Resource {
  f4rrknsp: string;  // label
  ced6vdp4: string;  // text
}

/** The actions `OpenedAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OPENEDANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.OpenedAnswer` kind at `path`. */
export function openedAnswer(ws: tilo.Workspace, path: string): OpenedAnswer {
  return resourceProxy(ws, path, SURFACE, OPENEDANSWER_ACTIONS) as unknown as OpenedAnswer;
}

/** DiffAsk — `tl.so.tilo.diffmerge.DiffAsk`. */
export interface DiffAsk extends tilo.Resource {
  vz6ogvrp: string;  // left
  "3upzelrr": string;  // right
}

/** The actions `DiffAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DIFFASK_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.DiffAsk` kind at `path`. */
export function diffAsk(ws: tilo.Workspace, path: string): DiffAsk {
  return resourceProxy(ws, path, SURFACE, DIFFASK_ACTIONS) as unknown as DiffAsk;
}

/** DiffAnswer — `tl.so.tilo.diffmerge.DiffAnswer`. */
export interface DiffAnswer extends tilo.Resource {
  hahxq35a: number[];  // block_firsts
  jx4fqkfc: number[];  // block_lasts
  we3fy74x: string[];  // block_kinds
  xmvhnvex: string[];  // block_previews
  fejavrie: string;  // left
  zmrjesu2: string;  // right
  psvy4e5w: string;  // rows
  akgopqc4: number;  // added
  "2wokc6qk": number;  // removed
  w6mvjrvf: number;  // modified
  frxu6oxq: boolean;  // identical
}

/** The actions `DiffAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DIFFANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.DiffAnswer` kind at `path`. */
export function diffAnswer(ws: tilo.Workspace, path: string): DiffAnswer {
  return resourceProxy(ws, path, SURFACE, DIFFANSWER_ACTIONS) as unknown as DiffAnswer;
}

/** MergeAsk — `tl.so.tilo.diffmerge.MergeAsk`. */
export interface MergeAsk extends tilo.Resource {
  vx52jl2g: string;  // base
  mrxlgkxk: string;  // ours
  nvyd6cgn: string;  // theirs
}

/** The actions `MergeAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MERGEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.MergeAsk` kind at `path`. */
export function mergeAsk(ws: tilo.Workspace, path: string): MergeAsk {
  return resourceProxy(ws, path, SURFACE, MERGEASK_ACTIONS) as unknown as MergeAsk;
}

/** MergeAnswer — `tl.so.tilo.diffmerge.MergeAnswer`. */
export interface MergeAnswer extends tilo.Resource {
  "2pwajyjm": number[];  // hunk_indices
  nyvwoswu: string[];  // hunk_resolutions
  imh4txwm: string[];  // hunk_bases
  fra5wt4r: string[];  // hunk_ours
  y56numrx: string[];  // hunk_theirs
  h6l3j5y3: string;  // merged
  e7auw454: number;  // changes
  oqot5haf: number;  // conflicts
}

/** The actions `MergeAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MERGEANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.MergeAnswer` kind at `path`. */
export function mergeAnswer(ws: tilo.Workspace, path: string): MergeAnswer {
  return resourceProxy(ws, path, SURFACE, MERGEANSWER_ACTIONS) as unknown as MergeAnswer;
}

/** ComposeAsk — `tl.so.tilo.diffmerge.ComposeAsk`. */
export interface ComposeAsk extends tilo.Resource {
  "55fy4avh": string;  // base
  fxqifsds: string;  // ours
  "6te7azkd": string;  // theirs
  atnd6awc: string[];  // choices
  weznhatv: string[];  // written
}

/** The actions `ComposeAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMPOSEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.ComposeAsk` kind at `path`. */
export function composeAsk(ws: tilo.Workspace, path: string): ComposeAsk {
  return resourceProxy(ws, path, SURFACE, COMPOSEASK_ACTIONS) as unknown as ComposeAsk;
}

/** ComposedAnswer — `tl.so.tilo.diffmerge.ComposedAnswer`. */
export interface ComposedAnswer extends tilo.Resource {
  wino2rjt: string;  // merged
  niqoebpg: number;  // changes
  gzqibeka: number;  // conflicts
}

/** The actions `ComposedAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMPOSEDANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.ComposedAnswer` kind at `path`. */
export function composedAnswer(ws: tilo.Workspace, path: string): ComposedAnswer {
  return resourceProxy(ws, path, SURFACE, COMPOSEDANSWER_ACTIONS) as unknown as ComposedAnswer;
}

/** FoldersAsk — `tl.so.tilo.diffmerge.FoldersAsk`. */
export interface FoldersAsk extends tilo.Resource {
  jasvo2t2: string;  // left
  "2go4z2dd": string;  // right
}

/** The actions `FoldersAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FOLDERSASK_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.FoldersAsk` kind at `path`. */
export function foldersAsk(ws: tilo.Workspace, path: string): FoldersAsk {
  return resourceProxy(ws, path, SURFACE, FOLDERSASK_ACTIONS) as unknown as FoldersAsk;
}

/** FoldersAnswer — `tl.so.tilo.diffmerge.FoldersAnswer`. */
export interface FoldersAnswer extends tilo.Resource {
  uytgg4wc: string;  // left_label
  bxame25h: string;  // left_root
  ewdcrys5: string;  // right_label
  ogpczjbg: string;  // right_root
  wb25c6a6: string[];  // entry_paths
  n2bmepxt: string[];  // entry_kinds
  bbb24qp5: number[];  // entry_left_lens
  mldndym5: number[];  // entry_right_lens
  "7nr427ip": boolean[];  // entry_by_lengths
  hqjkq6oq: boolean;  // truncated
}

/** The actions `FoldersAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FOLDERSANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.FoldersAnswer` kind at `path`. */
export function foldersAnswer(ws: tilo.Workspace, path: string): FoldersAnswer {
  return resourceProxy(ws, path, SURFACE, FOLDERSANSWER_ACTIONS) as unknown as FoldersAnswer;
}

/** SaveAsk — `tl.so.tilo.diffmerge.SaveAsk`. */
export interface SaveAsk extends tilo.Resource {
  t2ifxuue: string;  // path
  zoq67abl: string;  // text
}

/** The actions `SaveAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.SaveAsk` kind at `path`. */
export function saveAsk(ws: tilo.Workspace, path: string): SaveAsk {
  return resourceProxy(ws, path, SURFACE, SAVEASK_ACTIONS) as unknown as SaveAsk;
}

/** SavedAnswer — `tl.so.tilo.diffmerge.SavedAnswer`. */
export interface SavedAnswer extends tilo.Resource {
  tmjabrvg: string;  // name
}

/** The actions `SavedAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVEDANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.SavedAnswer` kind at `path`. */
export function savedAnswer(ws: tilo.Workspace, path: string): SavedAnswer {
  return resourceProxy(ws, path, SURFACE, SAVEDANSWER_ACTIONS) as unknown as SavedAnswer;
}

/** SampleAsk — `tl.so.tilo.diffmerge.SampleAsk`. */
export interface SampleAsk extends tilo.Resource {
  ol4drxwd: string;  // kind
}

/** The actions `SampleAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAMPLEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.SampleAsk` kind at `path`. */
export function sampleAsk(ws: tilo.Workspace, path: string): SampleAsk {
  return resourceProxy(ws, path, SURFACE, SAMPLEASK_ACTIONS) as unknown as SampleAsk;
}

/** SampleDiff — `tl.so.tilo.diffmerge.SampleDiff`. */
export interface SampleDiff extends tilo.Resource {
  uj6xvzvl: string;  // left_label
  cjwd2i2z: string;  // left
  vbhvxj6f: string;  // right_label
  whunzkq3: string;  // right
}

/** The actions `SampleDiff` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAMPLEDIFF_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.SampleDiff` kind at `path`. */
export function sampleDiff(ws: tilo.Workspace, path: string): SampleDiff {
  return resourceProxy(ws, path, SURFACE, SAMPLEDIFF_ACTIONS) as unknown as SampleDiff;
}

/** SampleMerge — `tl.so.tilo.diffmerge.SampleMerge`. */
export interface SampleMerge extends tilo.Resource {
  opwxdlzs: string;  // base_label
  l3kqbcv7: string;  // base
  wxfngzy5: string;  // ours_label
  ubqyr5rh: string;  // ours
  amhqg7cn: string;  // theirs_label
  wfaxbaim: string;  // theirs
}

/** The actions `SampleMerge` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAMPLEMERGE_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge.SampleMerge` kind at `path`. */
export function sampleMerge(ws: tilo.Workspace, path: string): SampleMerge {
  return resourceProxy(ws, path, SURFACE, SAMPLEMERGE_ACTIONS) as unknown as SampleMerge;
}

/** Diff & Merge — `tl.so.tilo.diffmerge`. */
export interface Diffmerge extends tilo.Resource {
}

/** The actions `Diffmerge` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DIFFMERGE_ACTIONS = [] as const;

/** The `tl.so.tilo.diffmerge` kind at `path`. */
export function diffmerge(ws: tilo.Workspace, path: string): Diffmerge {
  return resourceProxy(ws, path, SURFACE, DIFFMERGE_ACTIONS) as unknown as Diffmerge;
}

