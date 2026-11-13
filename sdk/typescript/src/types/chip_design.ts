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
 * says where it is.
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

/** Chip Design — `chip_design.project`. */
export interface Project extends tilo.Resource {
  readonly reports: Record<string, string>;
  readonly verifications: Record<string, string>;
  nodes: OperatorNode[];
  readonly connections: Connection[];
  importReport(args: {
    json: string;
  }): Promise<tilo.Run | void>;
  removeReport(args: {
    report: string;
  }): Promise<tilo.Run | void>;
  importVerification(args: {
    json: string;
  }): Promise<tilo.Run | void>;
  removeVerification(args: {
    report: string;
  }): Promise<tilo.Run | void>;
  addNode(args: {
    node: string;
    kind: OperatorKind;
    shape: number[];
    x: number;
    y: number;
    output_width?: number;
  }): Promise<tilo.Run | void>;
  setShape(args: {
    node: string;
    shape: number[];
  }): Promise<tilo.Run | void>;
  removeNode(args: {
    node: string;
  }): Promise<tilo.Run | void>;
  connect(args: {
    from_node: string;
    from_port: string;
    to_node: string;
    to_port: string;
  }): Promise<tilo.Run | void>;
  disconnect(args: {
    node: string;
    port: string;
  }): Promise<tilo.Run | void>;
}

/** The `chip_design.project` kind at `path`. */
export function project(ws: tilo.Workspace, path: string): Project {
  return resourceProxy(ws, path, SURFACE) as unknown as Project;
}

