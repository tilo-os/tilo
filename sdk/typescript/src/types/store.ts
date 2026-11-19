/**
 * Types for App Store (`so.tilo.store`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Catalog` is a `Resource` of the app's `store.catalog`, made by its factory with the
 * resource's path (`store/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.store@d903fd33913efeca";

export interface Report {
  status: string;
  note: string;
  case: string | null;
}

/** The arguments of `draft-report`. */
export interface DraftReportArgs {
  app_id: string;
}

/** The arguments of `record-moderation-case`. */
export interface RecordModerationCaseArgs {
  app_id: string;
  case_id: string;
  status: string;
}

/** App Store — `store.catalog`. */
export interface Catalog extends tilo.Resource {
  reports: Record<string, Report>;
  draftReport(args: {
    app_id: string;
  }): Promise<void>;
  /** Runs `draft-report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  draftReportMany(items: DraftReportArgs[]): Promise<tilo.ActionAnswer[]>;
  recordModerationCase(args: {
    app_id: string;
    case_id: string;
    status: string;
  }): Promise<void>;
  /** Runs `record-moderation-case` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordModerationCaseMany(items: RecordModerationCaseArgs[]): Promise<tilo.ActionAnswer[]>;
  inspectCatalogue(args?: {
    app?: string;
  }): Promise<void>;
  setSearch(args: {
    query: string;
  }): Promise<void>;
  showView(args: {
    view: string;
  }): Promise<void>;
  showMore(): Promise<void>;
  browseRemote(args: {
    remote: string;
  }): Promise<void>;
  setCategory(args: {
    category: string;
  }): Promise<void>;
  clearFilters(): Promise<void>;
  showDetails(args: {
    app: string;
  }): Promise<void>;
  showRuntimeDetails(args: {
    runtime: string;
    registry?: string;
  }): Promise<void>;
  setTechnicalDetails(args: {
    expanded: boolean;
  }): Promise<void>;
  openReference(args?: {
    path?: unknown;
    contents?: string;
  }): Promise<void>;
  showCatalogue(): Promise<void>;
  showRemotes(): Promise<void>;
  showTrust(): Promise<void>;
  prepareModeration(args: {
    app: string;
    appeal: boolean;
  }): Promise<void>;
}

/** The actions `Catalog` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CATALOG_ACTIONS = ["draftReport", "draftReportMany", "recordModerationCase", "recordModerationCaseMany", "inspectCatalogue", "setSearch", "showView", "showMore", "browseRemote", "setCategory", "clearFilters", "showDetails", "showRuntimeDetails", "setTechnicalDetails", "openReference", "showCatalogue", "showRemotes", "showTrust", "prepareModeration"] as const;

/** The `store.catalog` kind at `path`. */
export function catalog(ws: tilo.Workspace, path: string): Catalog {
  return resourceProxy(ws, path, SURFACE, CATALOG_ACTIONS) as unknown as Catalog;
}

