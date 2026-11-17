/**
 * Types for Diff & Merge (`so.tilo.diffmerge`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Session` is a `Resource` of the app's `diffmerge.session`, made by its factory with the
 * resource's path (`diffmerge/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one run.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.diffmerge@6e3e0e40ba1c3ff2";

export interface Comparison {
  left_label: string;
  left: string;
  right_label: string;
  right: string;
}

/** One of "ours", "theirs", "both". */
export type HunkChoice = "ours" | "theirs" | "both";

export interface MergeInputs {
  base_label: string;
  base: string;
  ours_label: string;
  ours: string;
  theirs_label: string;
  theirs: string;
}

/** The arguments of `open-comparison`. */
export interface OpenComparisonArgs {
  left_label: string;
  left: string;
  right_label: string;
  right: string;
}

/** The arguments of `open-merge`. */
export interface OpenMergeArgs {
  base_label: string;
  base: string;
  ours_label: string;
  ours: string;
  theirs_label: string;
  theirs: string;
}

/** The arguments of `take-ours`. */
export interface TakeOursArgs {
  hunk: number;
}

/** The arguments of `take-theirs`. */
export interface TakeTheirsArgs {
  hunk: number;
}

/** The arguments of `take-both`. */
export interface TakeBothArgs {
  hunk: number;
}

/** The arguments of `resolve`. */
export interface ResolveArgs {
  hunk: number;
  text: string;
}

/** Diff & Merge — `diffmerge.session`. */
export interface Session extends tilo.Resource {
  readonly comparison: Comparison | null;
  readonly merge: MergeInputs | null;
  merged: string;
  readonly resolutions: Record<string, HunkChoice>;
  readonly written: Record<string, string>;
  openComparison(args: {
    left_label: string;
    left: string;
    right_label: string;
    right: string;
  }): Promise<void>;
  /** Runs `open-comparison` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  openComparisonMany(items: OpenComparisonArgs[]): Promise<tilo.ActionAnswer[]>;
  openMerge(args: {
    base_label: string;
    base: string;
    ours_label: string;
    ours: string;
    theirs_label: string;
    theirs: string;
  }): Promise<void>;
  /** Runs `open-merge` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  openMergeMany(items: OpenMergeArgs[]): Promise<tilo.ActionAnswer[]>;
  takeOurs(args: {
    hunk: number;
  }): Promise<void>;
  /** Runs `take-ours` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  takeOursMany(items: TakeOursArgs[]): Promise<tilo.ActionAnswer[]>;
  takeTheirs(args: {
    hunk: number;
  }): Promise<void>;
  /** Runs `take-theirs` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  takeTheirsMany(items: TakeTheirsArgs[]): Promise<tilo.ActionAnswer[]>;
  takeBoth(args: {
    hunk: number;
  }): Promise<void>;
  /** Runs `take-both` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  takeBothMany(items: TakeBothArgs[]): Promise<tilo.ActionAnswer[]>;
  resolve(args: {
    hunk: number;
    text: string;
  }): Promise<void>;
  /** Runs `resolve` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  resolveMany(items: ResolveArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Session` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SESSION_ACTIONS = ["openComparison", "openComparisonMany", "openMerge", "openMergeMany", "takeOurs", "takeOursMany", "takeTheirs", "takeTheirsMany", "takeBoth", "takeBothMany", "resolve", "resolveMany"] as const;

/** The `diffmerge.session` kind at `path`. */
export function session(ws: tilo.Workspace, path: string): Session {
  return resourceProxy(ws, path, SURFACE, SESSION_ACTIONS) as unknown as Session;
}

