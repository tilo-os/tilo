/**
 * Types for Calculator (`so.tilo.calculator`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Calculation` is a `Resource` of the app's `tl.so.tilo.calculator.Calculation`, made by its factory with the
 * resource's path (`calculator/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.calculator@4adf6e79cc5a52de";

/** The arguments of `forget`. */
export interface ForgetArgs {
}

/** The arguments of `delete_curve`. */
export interface DeleteCurveArgs {
}

/** The arguments of `delete_surface`. */
export interface DeleteSurfaceArgs {
}

/** The arguments of `clear`. */
export interface ClearArgs {
}

/** Calculation — `tl.so.tilo.calculator.Calculation`. */
export interface Calculation extends tilo.Resource {
  "22vu26uc": string;  // expr
  y3tbavz4: string;  // result
  /** Takes one calculation out of the record. */
  forget(): Promise<void>;
  /** Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetMany(items: ForgetArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Calculation` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CALCULATION_ACTIONS = ["forget", "forgetMany"] as const;

/** The `tl.so.tilo.calculator.Calculation` kind at `path`. */
export function calculation(ws: tilo.Workspace, path: string): Calculation {
  return resourceProxy(ws, path, SURFACE, CALCULATION_ACTIONS) as unknown as Calculation;
}

/** Curve — `tl.so.tilo.calculator.Curve`. */
export interface Curve extends tilo.Resource {
  pxeb7d43: string;  // expr
  "5jdf7feh": boolean;  // visible
  /** Takes a curve off the 2D graph. */
  delete_curve(): Promise<void>;
  /** Runs `delete_curve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_curveMany(items: DeleteCurveArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Curve` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CURVE_ACTIONS = ["delete_curve", "delete_curveMany"] as const;

/** The `tl.so.tilo.calculator.Curve` kind at `path`. */
export function curve(ws: tilo.Workspace, path: string): Curve {
  return resourceProxy(ws, path, SURFACE, CURVE_ACTIONS) as unknown as Curve;
}

/** Surface — `tl.so.tilo.calculator.Surface`. */
export interface Surface extends tilo.Resource {
  ia2fz4sd: string;  // expr
  iuelnno5: boolean;  // visible
  /** Takes a surface off the 3D graph. */
  delete_surface(): Promise<void>;
  /** Runs `delete_surface` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_surfaceMany(items: DeleteSurfaceArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Surface` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SURFACE_ACTIONS = ["delete_surface", "delete_surfaceMany"] as const;

/** The `tl.so.tilo.calculator.Surface` kind at `path`. */
export function surface(ws: tilo.Workspace, path: string): Surface {
  return resourceProxy(ws, path, SURFACE, SURFACE_ACTIONS) as unknown as Surface;
}

/** Calculator — `tl.so.tilo.calculator.Calculator`. */
export interface Calculator extends tilo.Resource {
  tb3paq5e: Array<tilo.Entry<Calculation>>;  // calculations
  "7h4yz4ro": Array<tilo.Entry<Curve>>;  // curves
  zdfqnd3x: Array<tilo.Entry<Surface>>;  // surfaces
  /** Works out an expression and keeps the result in the record. The result is checked here, where the log records it with the expression that made it, and the display says the same number. */
  calculate(args: {
    expr: string;
    in_degrees?: boolean;
    answer?: number;
    memory?: number;
  }): Promise<Calculation>;
  /** Adds a curve to the 2D graph. */
  add_curve(args?: {
    formula?: string;
  }): Promise<Curve>;
  /** Adds a surface to the 3D graph. */
  add_surface(args?: {
    formula?: string;
  }): Promise<Surface>;
  /** Clears the record of everything worked out so far. */
  clear(): Promise<void>;
  /** Runs `clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clearMany(items: ClearArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Calculator` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CALCULATOR_ACTIONS = ["calculate", "add_curve", "add_surface", "clear", "clearMany"] as const;

/** The `tl.so.tilo.calculator.Calculator` kind at `path`. */
export function calculator(ws: tilo.Workspace, path: string): Calculator {
  return resourceProxy(ws, path, SURFACE, CALCULATOR_ACTIONS) as unknown as Calculator;
}

/** Calculator — `tl.so.tilo.calculator`. */
export interface TiloCalculator extends tilo.Resource {
  /** Puts `text` on the person's clipboard, as the system's copy does. */
  copy_text(args: {
    text: string;
  }): Promise<void>;
}

/** The actions `TiloCalculator` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOCALCULATOR_ACTIONS = ["copy_text"] as const;

/** The `tl.so.tilo.calculator` kind at `path`. */
export function tiloCalculator(ws: tilo.Workspace, path: string): TiloCalculator {
  return resourceProxy(ws, path, SURFACE, TILOCALCULATOR_ACTIONS) as unknown as TiloCalculator;
}

