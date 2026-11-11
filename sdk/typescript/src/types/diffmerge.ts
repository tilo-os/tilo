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
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** Diff & Merge — `diffmerge.session`. */
export interface Session extends tilo.Resource {
  readonly comparison: unknown;
  readonly merge: unknown;
  merged: unknown;
  readonly resolutions: unknown;
  readonly written: unknown;
  openComparison(args: {
    left_label: string;
    left: string;
    right_label: string;
    right: string;
  }): Promise<tilo.Run | void>;
  openMerge(args: {
    base_label: string;
    base: string;
    ours_label: string;
    ours: string;
    theirs_label: string;
    theirs: string;
  }): Promise<tilo.Run | void>;
  takeOurs(args: {
    hunk: number;
  }): Promise<tilo.Run | void>;
  takeTheirs(args: {
    hunk: number;
  }): Promise<tilo.Run | void>;
  takeBoth(args: {
    hunk: number;
  }): Promise<tilo.Run | void>;
  resolve(args: {
    hunk: number;
    text: string;
  }): Promise<tilo.Run | void>;
}

/** The `diffmerge.session` kind at `path`. */
export function session(ws: tilo.Workspace, path: string): Session {
  return resourceProxy(ws, path) as unknown as Session;
}

