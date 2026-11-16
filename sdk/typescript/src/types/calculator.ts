/**
 * Types for Calculator (`so.tilo.calculator`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Document` is a `Resource` of the app's `calculator.document`, made by its factory with the
 * resource's path (`calculator/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.calculator@272ee377b4cb7c5b";

export interface HistoryEntry {
  expr: string;
  result: string;
}

export interface PlotFn {
  expr: string;
  visible: boolean;
}

/** The arguments of `commit-history`. */
export interface CommitHistoryArgs {
  expr: string;
  result: string;
}

/** The arguments of `delete-history-entry`. */
export interface DeleteHistoryEntryArgs {
  entry: string;
}

/** The arguments of `clear-history`. */
export interface ClearHistoryArgs {
}

/** The arguments of `add-function`. */
export interface AddFunctionArgs {
  formula: string;
}

/** The arguments of `add-surface`. */
export interface AddSurfaceArgs {
  formula: string;
}

/** The arguments of `delete-function`. */
export interface DeleteFunctionArgs {
  function: string;
}

/** The arguments of `delete-surface`. */
export interface DeleteSurfaceArgs {
  surface: string;
}

/** Calculator — `calculator.document`. */
export interface Document extends tilo.Resource {
  readonly history: HistoryEntry[];
  funcs: PlotFn[];
  surfaces: PlotFn[];
  commitHistory(args: {
    expr: string;
    result: string;
  }): Promise<tilo.Run | void>;
  /** Runs `commit-history` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  commitHistoryMany(items: CommitHistoryArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteHistoryEntry(args: {
    entry: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-history-entry` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteHistoryEntryMany(items: DeleteHistoryEntryArgs[]): Promise<tilo.ActionAnswer[]>;
  clearHistory(): Promise<tilo.Run | void>;
  /** Runs `clear-history` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  clearHistoryMany(items: ClearHistoryArgs[]): Promise<tilo.ActionAnswer[]>;
  addFunction(args: {
    formula: string;
  }): Promise<tilo.Run | void>;
  /** Runs `add-function` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addFunctionMany(items: AddFunctionArgs[]): Promise<tilo.ActionAnswer[]>;
  addSurface(args: {
    formula: string;
  }): Promise<tilo.Run | void>;
  /** Runs `add-surface` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addSurfaceMany(items: AddSurfaceArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteFunction(args: {
    function: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-function` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteFunctionMany(items: DeleteFunctionArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteSurface(args: {
    surface: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-surface` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteSurfaceMany(items: DeleteSurfaceArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The `calculator.document` kind at `path`. */
export function calculatorDocument(ws: tilo.Workspace, path: string): Document {
  return resourceProxy(ws, path, SURFACE) as unknown as Document;
}

