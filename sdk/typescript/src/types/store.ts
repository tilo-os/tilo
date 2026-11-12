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
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "@tilo/sdk";
import type * as tilo from "@tilo/sdk";

export interface Report {
  status: string;
  note: string;
  case: string | null;
}

/** App Store — `store.catalog`. */
export interface Catalog extends tilo.Resource {
  reports: Record<string, Report>;
  draftReport(args: {
    app_id: string;
  }): Promise<tilo.Run | void>;
  recordModerationCase(args: {
    app_id: string;
    case_id: string;
    status: string;
  }): Promise<tilo.Run | void>;
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

/** The `store.catalog` kind at `path`. */
export function catalog(ws: tilo.Workspace, path: string): Catalog {
  return resourceProxy(ws, path) as unknown as Catalog;
}

