/**
 * Types for the apps a workspace has, written by `tilo sdk gen`.
 *
 * `tilo sdk gen --lang ts -w <workspace>` writes one interface per resource
 * kind here, fields and one method per action over `Resource`. Until then
 * every resource still works through `ws.resource(path)` and
 * `ws.do(path, args)`.
 */

import type { Resource } from "../resource.ts";

/** A resource of one kind, as the catalogue declares it. */
export type Typed<T> = Resource & T;
