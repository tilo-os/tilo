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
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

export interface HistoryEntry {
  expr: string;
  result: string;
}

export interface PlotFn {
  expr: string;
  visible: boolean;
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
  deleteHistoryEntry(args: {
    entry: string;
  }): Promise<tilo.Run | void>;
  clearHistory(): Promise<tilo.Run | void>;
  addFunction(args: {
    formula: string;
  }): Promise<tilo.Run | void>;
  addSurface(args: {
    formula: string;
  }): Promise<tilo.Run | void>;
  deleteFunction(args: {
    function: string;
  }): Promise<tilo.Run | void>;
  deleteSurface(args: {
    surface: string;
  }): Promise<tilo.Run | void>;
}

/** The `calculator.document` kind at `path`. */
export function calculatorDocument(ws: tilo.Workspace, path: string): Document {
  return resourceProxy(ws, path) as unknown as Document;
}

