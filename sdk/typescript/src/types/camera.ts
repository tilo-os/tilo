/**
 * Types for Camera (`so.tilo.camera`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Camera` is a `Resource` of the app's `camera`, made by its factory with the
 * resource's path (`camera/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.camera@4fefa4db144ed22b";

/** One of "Photo", "Video". */
export type CaptureKind = "Photo" | "Video";

/** One of "Back", "Front", "Wide". */
export type Facing = "Back" | "Front" | "Wide";

export interface ScanEntry {
  raw: string;
  ts: number;
}

export interface SharedCapture {
  id: string;
  media_id: string;
  kind: CaptureKind;
  ts: number;
  w: number;
  h: number;
  dur_ms: number;
  label: string;
  facing: Facing;
}

/** The arguments of `register-capture`. */
export interface RegisterCaptureArgs {
  capture: SharedCapture;
}

/** The arguments of `record-scan`. */
export interface RecordScanArgs {
  raw: string;
}

/** Camera — `camera`. */
export interface Camera extends tilo.Resource {
  readonly scans: ScanEntry[];
  readonly recent: SharedCapture | null;
  registerCapture(args: {
    capture: SharedCapture;
  }): Promise<void>;
  /** Runs `register-capture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  registerCaptureMany(items: RegisterCaptureArgs[]): Promise<tilo.ActionAnswer[]>;
  recordScan(args: {
    raw: string;
  }): Promise<void>;
  /** Runs `record-scan` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordScanMany(items: RecordScanArgs[]): Promise<tilo.ActionAnswer[]>;
  setGrid(args: {
    on: boolean;
  }): Promise<void>;
  setLevel(args: {
    on: boolean;
  }): Promise<void>;
}

/** The actions `Camera` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CAMERA_ACTIONS = ["registerCapture", "registerCaptureMany", "recordScan", "recordScanMany", "setGrid", "setLevel"] as const;

/** The `camera` kind at `path`. */
export function camera(ws: tilo.Workspace, path: string): Camera {
  return resourceProxy(ws, path, SURFACE, CAMERA_ACTIONS) as unknown as Camera;
}

