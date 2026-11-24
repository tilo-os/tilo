/**
 * Types for Firewall (`so.tilo.firewall`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`firewall/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.firewall@dea07c56fb981527";

/** The arguments of `import_device`. */
export interface ImportDeviceArgs {
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

/** Rule — `tl.so.tilo.firewall.Rule`. */
export interface Rule extends tilo.Resource {
  eeqhnuyu: string;  // title
  "3jndsc7i": string;  // service
  dlj2kbdr: string;  // port
  nconpw3q: string;  // proto
  "2ov2rg7l": string;  // verdict
  adadcwgr: string;  // source
  td35s7rd: boolean;  // enabled
}

/** The actions `Rule` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RULE_ACTIONS = [] as const;

/** The `tl.so.tilo.firewall.Rule` kind at `path`. */
export function rule(ws: tilo.Workspace, path: string): Rule {
  return resourceProxy(ws, path, SURFACE, RULE_ACTIONS) as unknown as Rule;
}

/** Zone — `tl.so.tilo.firewall.Zone`. */
export interface Zone extends tilo.Resource {
  kzbzenxe: string;  // kind
  xthxweeb: string;  // target
  "6oz67tou": string[];  // interfaces
  fcur4ope: string[];  // services
  vyfun5ze: Array<tilo.Entry<Rule>>;  // rules
  bqtwzcsu: boolean;  // masquerade
}

/** The actions `Zone` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ZONE_ACTIONS = [] as const;

/** The `tl.so.tilo.firewall.Zone` kind at `path`. */
export function zone(ws: tilo.Workspace, path: string): Zone {
  return resourceProxy(ws, path, SURFACE, ZONE_ACTIONS) as unknown as Zone;
}

/** Firewall — `tl.so.tilo.firewall.Firewall`. */
export interface Firewall extends tilo.Resource {
  fzjbpclp: boolean;  // enabled
  lsn43itt: boolean;  // panic_mode
  qzperbb7: string;  // default_zone
  fjnbtnf2: Array<tilo.Entry<Zone>>;  // zones
  /** Applies the workspace's configuration to this device's firewalld when it does not hold it: a change another device made, or one taken back, reaches this device's firewall too. */
  apply_stored(): Promise<void>;
  /** Brings in what this device's firewalld holds: the zones, their services and rules, and the switches. A device whose firewalld does not answer keeps the configuration it has. */
  import_device(): Promise<void>;
  /** Runs `import_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_deviceMany(items: ImportDeviceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts firewalld. */
  start(): Promise<void>;
  /** Stops firewalld. Undo starts it again but cannot take back what reached the machine while it was open, so this is asked first. */
  stop(): Promise<void>;
  /** Enters panic mode: all traffic in and out is dropped. Undo lifts it but cannot restore the connections it cut, so this is asked first. */
  enter_panic(): Promise<void>;
  /** Leaves panic mode. */
  exit_panic(): Promise<void>;
  /** Makes a zone the active one. */
  make_active(args: {
    zone: string;
  }): Promise<void>;
  /** Turns a rule on or off in a zone. */
  toggle_rule(args: {
    zone: string;
    rule: string;
    on: boolean;
  }): Promise<void>;
  /** Allows a service in a zone, or stops allowing it. */
  allow_service(args: {
    zone: string;
    service: string;
    on: boolean;
  }): Promise<void>;
  /** Adds a rule to a zone, or saves the one being edited. A rule that names no valid port or source is refused, and the form says why. */
  add_rule(args: {
    zone: string;
    editing: string;
    as_service: boolean;
    service: string;
    port: string;
    proto: string;
    verdict: string;
    source: string;
  }): Promise<void>;
  /** Deletes a rule from a zone. */
  delete_rule(args: {
    zone: string;
    rule: string;
  }): Promise<void>;
}

/** The actions `Firewall` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FIREWALL_ACTIONS = ["apply_stored", "import_device", "import_deviceMany", "start", "stop", "enter_panic", "exit_panic", "make_active", "toggle_rule", "allow_service", "add_rule", "delete_rule"] as const;

/** The `tl.so.tilo.firewall.Firewall` kind at `path`. */
export function firewall(ws: tilo.Workspace, path: string): Firewall {
  return resourceProxy(ws, path, SURFACE, FIREWALL_ACTIONS) as unknown as Firewall;
}

/** ApplyConfig — `tl.so.tilo.firewall.ApplyConfig`. */
export interface ApplyConfig extends tilo.Resource {
  z4rwsmkt: boolean;  // enabled
  hfx4peji: boolean;  // panic_mode
  wpzvhcny: string;  // default_zone
  fa3zy4tg: ApplyZone[];  // zones
}

/** The actions `ApplyConfig` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const APPLYCONFIG_ACTIONS = [] as const;

/** The `tl.so.tilo.firewall.ApplyConfig` kind at `path`. */
export function applyConfig(ws: tilo.Workspace, path: string): ApplyConfig {
  return resourceProxy(ws, path, SURFACE, APPLYCONFIG_ACTIONS) as unknown as ApplyConfig;
}

/** ApplyZone — `tl.so.tilo.firewall.ApplyZone`. */
export interface ApplyZone extends tilo.Resource {
  ki3bym3t: string;  // kind
  npflzozj: string;  // target
  yll5vuns: string[];  // interfaces
  kasq6ajf: string[];  // services
  rsyraw6p: ApplyRule[];  // rules
  mi6vac6j: boolean;  // masquerade
}

/** The actions `ApplyZone` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const APPLYZONE_ACTIONS = [] as const;

/** The `tl.so.tilo.firewall.ApplyZone` kind at `path`. */
export function applyZone(ws: tilo.Workspace, path: string): ApplyZone {
  return resourceProxy(ws, path, SURFACE, APPLYZONE_ACTIONS) as unknown as ApplyZone;
}

/** ApplyRule — `tl.so.tilo.firewall.ApplyRule`. */
export interface ApplyRule extends tilo.Resource {
  iddy2jy5: string;  // service
  rukhcwu4: string;  // port
  "46qdyqd7": string;  // proto
  b7tu4bhi: string;  // verdict
  v2uf2efw: string;  // source
  pqajeuq5: boolean;  // enabled
}

/** The actions `ApplyRule` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const APPLYRULE_ACTIONS = [] as const;

/** The `tl.so.tilo.firewall.ApplyRule` kind at `path`. */
export function applyRule(ws: tilo.Workspace, path: string): ApplyRule {
  return resourceProxy(ws, path, SURFACE, APPLYRULE_ACTIONS) as unknown as ApplyRule;
}

/** ServiceDef — `tl.so.tilo.firewall.ServiceDef`. */
export interface ServiceDef extends tilo.Resource {
  splkq5cx: string;  // key
  um6wchuo: string;  // name
  "3lp2tu4x": string;  // ports
  lr6vcvpw: string;  // proto
  dguxp5s2: string;  // description
}

/** The actions `ServiceDef` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SERVICEDEF_ACTIONS = [] as const;

/** The `tl.so.tilo.firewall.ServiceDef` kind at `path`. */
export function serviceDef(ws: tilo.Workspace, path: string): ServiceDef {
  return resourceProxy(ws, path, SURFACE, SERVICEDEF_ACTIONS) as unknown as ServiceDef;
}

/** Firewall — `tl.so.tilo.firewall`. */
export interface TiloFirewall extends tilo.Resource {
  /** Reads what the kernel dropped in the last hour. */
  load_blocked(): Promise<void>;
}

/** The actions `TiloFirewall` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOFIREWALL_ACTIONS = ["load_blocked"] as const;

/** The `tl.so.tilo.firewall` kind at `path`. */
export function tiloFirewall(ws: tilo.Workspace, path: string): TiloFirewall {
  return resourceProxy(ws, path, SURFACE, TILOFIREWALL_ACTIONS) as unknown as TiloFirewall;
}

