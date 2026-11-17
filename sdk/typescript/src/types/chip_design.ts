/**
 * Types for Chip Design (`so.tilo.chip-design`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Project` is a `Resource` of the app's `chip_design.project`, made by its factory with the
 * resource's path (`chip-design/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.chip-design@fa1a68b0cfa3e301";

export interface Connection {
  from_node: string;
  from_port: string;
  to_node: string;
  to_port: string;
}

/** One of "input", "gate_input", "norm_weight_input", "rms_norm", "apply_norm_weight", "silu", "multiply", "projection_input", "matrix_weight_input", "linear_projection", "add". */
export type OperatorKind = "input" | "gate_input" | "norm_weight_input" | "rms_norm" | "apply_norm_weight" | "silu" | "multiply" | "projection_input" | "matrix_weight_input" | "linear_projection" | "add";

export interface OperatorNode {
  id: string;
  kind: OperatorKind;
  shape: number[];
  normalization_width: number;
  output_width: number | null;
  x: number;
  y: number;
}

/** The arguments of `import-report`. */
export interface ImportReportArgs {
  json: string;
}

/** The arguments of `remove-report`. */
export interface RemoveReportArgs {
  report: string;
}

/** The arguments of `import-verification`. */
export interface ImportVerificationArgs {
  json: string;
}

/** The arguments of `remove-verification`. */
export interface RemoveVerificationArgs {
  report: string;
}

/** The arguments of `add-node`. */
export interface AddNodeArgs {
  node: string;
  kind: OperatorKind;
  shape: number[];
  x: number;
  y: number;
  output_width?: number;
}

/** The arguments of `set-shape`. */
export interface SetShapeArgs {
  node: string;
  shape: number[];
}

/** The arguments of `remove-node`. */
export interface RemoveNodeArgs {
  node: string;
}

/** The arguments of `connect`. */
export interface ConnectArgs {
  from_node: string;
  from_port: string;
  to_node: string;
  to_port: string;
}

/** The arguments of `disconnect`. */
export interface DisconnectArgs {
  node: string;
  port: string;
}

/** Chip Design — `chip_design.project`. */
export interface Project extends tilo.Resource {
  readonly reports: Record<string, string>;
  readonly verifications: Record<string, string>;
  nodes: Array<tilo.Entry<OperatorNode>>;
  readonly connections: Connection[];
  importReport(args: {
    json: string;
  }): Promise<void>;
  /** Runs `import-report` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  importReportMany(items: ImportReportArgs[]): Promise<tilo.ActionAnswer[]>;
  removeReport(args: {
    report: string;
  }): Promise<void>;
  /** Runs `remove-report` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeReportMany(items: RemoveReportArgs[]): Promise<tilo.ActionAnswer[]>;
  importVerification(args: {
    json: string;
  }): Promise<void>;
  /** Runs `import-verification` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  importVerificationMany(items: ImportVerificationArgs[]): Promise<tilo.ActionAnswer[]>;
  removeVerification(args: {
    report: string;
  }): Promise<void>;
  /** Runs `remove-verification` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeVerificationMany(items: RemoveVerificationArgs[]): Promise<tilo.ActionAnswer[]>;
  addNode(args: {
    node: string;
    kind: OperatorKind;
    shape: number[];
    x: number;
    y: number;
    output_width?: number;
  }): Promise<void>;
  /** Runs `add-node` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addNodeMany(items: AddNodeArgs[]): Promise<tilo.ActionAnswer[]>;
  setShape(args: {
    node: string;
    shape: number[];
  }): Promise<void>;
  /** Runs `set-shape` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setShapeMany(items: SetShapeArgs[]): Promise<tilo.ActionAnswer[]>;
  removeNode(args: {
    node: string;
  }): Promise<void>;
  /** Runs `remove-node` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeNodeMany(items: RemoveNodeArgs[]): Promise<tilo.ActionAnswer[]>;
  connect(args: {
    from_node: string;
    from_port: string;
    to_node: string;
    to_port: string;
  }): Promise<void>;
  /** Runs `connect` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  connectMany(items: ConnectArgs[]): Promise<tilo.ActionAnswer[]>;
  disconnect(args: {
    node: string;
    port: string;
  }): Promise<void>;
  /** Runs `disconnect` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  disconnectMany(items: DisconnectArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Project` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROJECT_ACTIONS = ["importReport", "importReportMany", "removeReport", "removeReportMany", "importVerification", "importVerificationMany", "removeVerification", "removeVerificationMany", "addNode", "addNodeMany", "setShape", "setShapeMany", "removeNode", "removeNodeMany", "connect", "connectMany", "disconnect", "disconnectMany"] as const;

/** The `chip_design.project` kind at `path`. */
export function project(ws: tilo.Workspace, path: string): Project {
  return resourceProxy(ws, path, SURFACE, PROJECT_ACTIONS) as unknown as Project;
}

