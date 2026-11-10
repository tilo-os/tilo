/**
 * Types for Firewall (`so.tilo.firewall`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Config` is a `Resource` of the app's `firewall.config`, made by its factory with the
 * resource's path (`firewall/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "@tilo/sdk";
import type * as tilo from "@tilo/sdk";

/** One of "allow", "deny". */
export type Action = "allow" | "deny";

/** One of service, port: an object whose case the app tags it with. */
export type Matcher = Record<string, unknown>;

/** One of "tcp", "udp". */
export type Proto = "tcp" | "udp";

export interface StoredRule {
  matcher: Matcher;
  action: Action;
  source: string;
  enabled: boolean;
}

export interface StoredZone {
  target: Target;
  interfaces: string[];
  services: string[];
  rules: StoredRule[];
  masquerade: boolean;
  deleted_rules: string[];
}

/** One of "default", "accept", "reject", "drop". */
export type Target = "default" | "accept" | "reject" | "drop";

/** One of "drop", "block", "public", "external", "dmz", "work", "home", "internal", "trusted". */
export type ZoneKind = "drop" | "block" | "public" | "external" | "dmz" | "work" | "home" | "internal" | "trusted";

/** Firewall — `firewall.config`. */
export interface Config extends tilo.Resource {
  readonly enabled: boolean;
  readonly panic_mode: boolean;
  default_zone: ZoneKind;
  zones: Record<string, StoredZone>;
  start(): Promise<tilo.Run | void>;
  stop(): Promise<tilo.Run | void>;
  enterPanicMode(): Promise<tilo.Run | void>;
  exitPanicMode(): Promise<tilo.Run | void>;
  deleteRule(args: {
    zone: ZoneKind;
    rule: string;
  }): Promise<tilo.Run | void>;
  import(args: {
    config: unknown;
  }): Promise<tilo.Run | void>;
  readBlocked(): Promise<void>;
}

/** The `firewall.config` kind at `path`. */
export function config(ws: tilo.Workspace, path: string): Config {
  return resourceProxy(ws, path) as unknown as Config;
}

/** Applied configuration — `firewall.applied`. */
export interface Applied extends tilo.Resource {
  devices: Record<string, string>;
  applied(args: {
    device: string;
    config: string;
  }): Promise<tilo.Run | void>;
}

/** The `firewall.applied` kind at `path`. */
export function applied(ws: tilo.Workspace, path: string): Applied {
  return resourceProxy(ws, path) as unknown as Applied;
}

