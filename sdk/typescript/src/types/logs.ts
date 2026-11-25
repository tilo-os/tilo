/**
 * Types for Logs (`so.tilo.logs`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`logs/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.logs@8f6a67d584534406";

/** The arguments of `set_severity`. */
export interface SetSeverityArgs {
  value: string;
}

/** The arguments of `set_unit`. */
export interface SetUnitArgs {
  value: string;
}

/** The arguments of `set_boot`. */
export interface SetBootArgs {
  value: string;
}

/** The arguments of `set_search`. */
export interface SetSearchArgs {
  value: string;
}

/** The arguments of `choose`. */
export interface ChooseArgs {
  key: string;
}

/** The arguments of `clear_filters`. */
export interface ClearFiltersArgs {
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

/** Logs — `tl.so.tilo.logs.Logs`. */
export interface Logs extends tilo.Resource {
  "6e5izmwp": string;  // severity
  lq27tlwx: string;  // unit
  vzywk5iy: string;  // boot
  yuygrdvc: string;  // search
  rumvop3c: boolean;  // following
  cctmzqe2: string;  // selected
  /** The records an export writes, one JSON object a line. */
  made_export(): Promise<string>;
  /** Shows records of at least `value`'s severity. */
  set_severity(args: {
    value: string;
  }): Promise<void>;
  /** Runs `set_severity` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_severityMany(items: SetSeverityArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one unit's records, or every unit's when `value` is empty. */
  set_unit(args: {
    value: string;
  }): Promise<void>;
  /** Runs `set_unit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_unitMany(items: SetUnitArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one boot's records, or every boot's when `value` is empty. */
  set_boot(args: {
    value: string;
  }): Promise<void>;
  /** Runs `set_boot` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_bootMany(items: SetBootArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Narrows the records to those holding `value`. */
  set_search(args: {
    value: string;
  }): Promise<void>;
  /** Runs `set_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_searchMany(items: SetSearchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one record's detail. */
  choose(args: {
    key: string;
  }): Promise<void>;
  /** Runs `choose` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  chooseMany(items: ChooseArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Clears every journal filter. */
  clear_filters(): Promise<void>;
  /** Runs `clear_filters` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_filtersMany(items: ClearFiltersArgs[]): Promise<tilo.ActionAnswer[]>;
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  /** Follows new records as they arrive, or stops following them. */
  follow(args: {
    on: boolean;
  }): Promise<void>;
  /** Reads the records written since the last one read, while following. */
  poll_tail(): Promise<void>;
}

/** The actions `Logs` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LOGS_ACTIONS = ["made_export", "set_severity", "set_severityMany", "set_unit", "set_unitMany", "set_boot", "set_bootMany", "set_search", "set_searchMany", "choose", "chooseMany", "clear_filters", "clear_filtersMany", "follow", "poll_tail"] as const;

/** The `tl.so.tilo.logs.Logs` kind at `path`. */
export function logs(ws: tilo.Workspace, path: string): Logs {
  return resourceProxy(ws, path, SURFACE, LOGS_ACTIONS) as unknown as Logs;
}

/** ExportEntry — `tl.so.tilo.logs.ExportEntry`. */
export interface ExportEntry extends tilo.Resource {
  "37kvzek7": number;  // realtime_us
  v3wlyikw: string;  // unit
  ur4aajno: string;  // identifier
  wsg4pkbu: number;  // priority
  cd6taqqw: string;  // message
  xkxlpyrt: number | null;  // pid
  "5wwthj2f": string;  // hostname
  odiofhx6: string;  // boot_id
  "3ra7d3km": string;  // transport
  djjluyru: ExportField[];  // extra
}

/** The actions `ExportEntry` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EXPORTENTRY_ACTIONS = [] as const;

/** The `tl.so.tilo.logs.ExportEntry` kind at `path`. */
export function exportEntry(ws: tilo.Workspace, path: string): ExportEntry {
  return resourceProxy(ws, path, SURFACE, EXPORTENTRY_ACTIONS) as unknown as ExportEntry;
}

/** ExportField — `tl.so.tilo.logs.ExportField`. */
export interface ExportField extends tilo.Resource {
  mj25pae6: string;  // name
  pnqueidz: string;  // value
}

/** The actions `ExportField` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EXPORTFIELD_ACTIONS = [] as const;

/** The `tl.so.tilo.logs.ExportField` kind at `path`. */
export function exportField(ws: tilo.Workspace, path: string): ExportField {
  return resourceProxy(ws, path, SURFACE, EXPORTFIELD_ACTIONS) as unknown as ExportField;
}

/** Logs — `tl.so.tilo.logs`. */
export interface TiloLogs extends tilo.Resource {
  /** Copies one record, with its time, unit, program and message. */
  copy_entry(args: {
    entry: unknown;
  }): Promise<void>;
  /** Copies one record's message. */
  copy_message(args: {
    entry: unknown;
  }): Promise<void>;
}

/** The actions `TiloLogs` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOLOGS_ACTIONS = ["copy_entry", "copy_message"] as const;

/** The `tl.so.tilo.logs` kind at `path`. */
export function tiloLogs(ws: tilo.Workspace, path: string): TiloLogs {
  return resourceProxy(ws, path, SURFACE, TILOLOGS_ACTIONS) as unknown as TiloLogs;
}

