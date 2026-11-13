/**
 * Types for Shortcuts (`so.tilo.shortcuts`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Workbench` is a `Resource` of the app's `shortcuts.workbench`, made by its factory with the
 * resource's path (`shortcuts/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.shortcuts@a41479e69568cbd8";

/** Shortcuts — `shortcuts.workbench`. */
export interface Workbench extends tilo.Resource {
  drafts: unknown;
  ignored: unknown;
  draftProposal(args: {
    shortcut: unknown;
  }): Promise<tilo.Run | void>;
  ignoreProposal(args: {
    key: string;
  }): Promise<tilo.Run | void>;
  allowProposal(args: {
    key: string;
  }): Promise<tilo.Run | void>;
  newDraft(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  duplicateDraft(args: {
    draft: string;
  }): Promise<tilo.Run | void>;
  draftShortcut(args: {
    shortcut: unknown;
  }): Promise<tilo.Run | void>;
  openShortcut(args: {
    shortcut: unknown;
  }): Promise<tilo.Run | void>;
  duplicateShortcut(args: {
    shortcut: unknown;
  }): Promise<tilo.Run | void>;
  setSchedule(args: {
    draft: string;
    enabled: boolean;
    hour: number;
    minute: number;
  }): Promise<tilo.Run | void>;
  discard(args: {
    draft: string;
  }): Promise<tilo.Run | void>;
  addStep(args: {
    draft: string;
    block: string;
    step: unknown;
  }): Promise<tilo.Run | void>;
  replaceStep(args: {
    draft: string;
    path: string;
    step: unknown;
  }): Promise<tilo.Run | void>;
  bindArgument(args: {
    draft: string;
    path: string;
    param: string;
    binding: string;
  }): Promise<tilo.Run | void>;
  removeStep(args: {
    draft: string;
    path: string;
  }): Promise<tilo.Run | void>;
  moveStep(args: {
    draft: string;
    path: string;
    earlier: boolean;
  }): Promise<tilo.Run | void>;
  setStepPolicy(args: {
    draft: string;
    path: string;
    policy: string;
  }): Promise<tilo.Run | void>;
  setNested(args: {
    draft: string;
    path: string;
    block: string;
    index: number;
  }): Promise<tilo.Run | void>;
  setOutput(args: {
    draft: string;
    binding: string;
  }): Promise<tilo.Run | void>;
  addInput(args: {
    draft: string;
    name: string;
    kind: string;
  }): Promise<tilo.Run | void>;
  removeInput(args: {
    draft: string;
    name: string;
  }): Promise<tilo.Run | void>;
}

/** The `shortcuts.workbench` kind at `path`. */
export function workbench(ws: tilo.Workspace, path: string): Workbench {
  return resourceProxy(ws, path, SURFACE) as unknown as Workbench;
}

