/**
 * Types for Remote Desktop (`so.tilo.remote`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`remote/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.remote@84ce024c9aff232a";

/** How a connection signs in. The secret itself is never here: a password or a key's passphrase is kept by the device that typed it. One of "password", "public_key", "device_identity". */
export type AuthMethod = "password" | "public_key" | "device_identity";

/** What a saved connection speaks. One of "ssh", "vnc", "rdp", "tilo". */
export type ConnectionKind = "ssh" | "vnc" | "rdp" | "tilo";

/** The arguments of `change_connection`. */
export interface ChangeConnectionArgs {
  name: string;
  host: string;
  kind: ConnectionKind;
  port: number;
  user: string;
  auth: AuthMethod;
  group: string;
}

/** The arguments of `pin`. */
export interface PinArgs {
  pinned?: boolean;
}

/** The arguments of `keep_secret`. */
export interface KeepSecretArgs {
  secret: string;
}

/** The arguments of `mark_used`. */
export interface MarkUsedArgs {
}

/** The arguments of `disconnect`. */
export interface DisconnectArgs {
}

/** The arguments of `trust_host_key`. */
export interface TrustHostKeyArgs {
  fingerprint: string;
}

/** The arguments of `forget_host_key`. */
export interface ForgetHostKeyArgs {
}

/** The arguments of `delete_connection`. */
export interface DeleteConnectionArgs {
  c: Connection;
}

/** The arguments of `close_session`. */
export interface CloseSessionArgs {
}

/** The arguments of `observe`. */
export interface ObserveArgs {
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

/** Connection — `tl.so.tilo.remote.Connection`. */
export interface Connection extends tilo.Resource {
  ssofhrgw: string;  // name
  dyz6c4fy: string;  // host
  "3zbula3z": number;  // port
  qudrxklp: string;  // user
  auxqrphf: ConnectionKind;  // kind
  e54cvtei: AuthMethod;  // auth
  jf7npid4: string | null;  // last_used
  poxhapg7: string;  // group
  qjgd4xi7: boolean;  // favorite
  /** Changes what a saved connection is: only what differs is written, so two people editing one connection keep each other's changes. */
  change_connection(args: {
    name: string;
    host: string;
    kind: ConnectionKind;
    port: number;
    user: string;
    auth: AuthMethod;
    group: string;
  }): Promise<void>;
  /** Runs `change_connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  change_connectionMany(items: ChangeConnectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Pins a connection to the top of the list, or unpins it. */
  pin(args?: {
    pinned?: boolean;
  }): Promise<void>;
  /** Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pinMany(items: PinArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps the password or the key's passphrase this device signs in to `c` with; an empty one forgets it. It never leaves this device. */
  keep_secret(args: {
    secret: string;
  }): Promise<void>;
  /** Runs `keep_secret` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_secretMany(items: KeepSecretArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Notes that a session to `c` was opened now. */
  mark_used(): Promise<void>;
  /** Runs `mark_used` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  mark_usedMany(items: MarkUsedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens a live session to a saved connection: a terminal for SSH, the machine's screen for VNC. Nothing is sent before the saved address and sign-in are checked again. */
  connect(): Promise<Session>;
  /** Ends the session to `c` this device has open. */
  disconnect(): Promise<void>;
  /** Runs `disconnect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  disconnectMany(items: DisconnectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Types `text` into an open SSH session, as if it were typed at its terminal. */
  send_text(args: {
    text: string;
  }): Promise<void>;
  /** Sends one file from this device to the machine an SSH session is open to. */
  put_file(args: {
    local_path: string;
    remote_path: string;
  }): Promise<void>;
  /** Fetches one file from the machine an SSH session is open to, onto this device. */
  fetch_file(args: {
    remote_path: string;
    local_path: string;
  }): Promise<void>;
  /** Says how a file's way to or from a machine ended. */
  moved_file(args: {
    j: unknown;
  }): Promise<void>;
  /** Trusts the SSH key a machine showed on first contact, on this device only; connecting again signs in. The key is checked against the one shown: a machine that changed it since is asked about again. */
  trust_host_key(args: {
    fingerprint: string;
  }): Promise<void>;
  /** Runs `trust_host_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  trust_host_keyMany(items: TrustHostKeyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Forgets the key this device trusted for a machine whose key changed; connecting again shows the new one to check. */
  forget_host_key(): Promise<void>;
  /** Runs `forget_host_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forget_host_keyMany(items: ForgetHostKeyArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Connection` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONNECTION_ACTIONS = ["change_connection", "change_connectionMany", "pin", "pinMany", "keep_secret", "keep_secretMany", "mark_used", "mark_usedMany", "connect", "disconnect", "disconnectMany", "send_text", "put_file", "fetch_file", "moved_file", "trust_host_key", "trust_host_keyMany", "forget_host_key", "forget_host_keyMany"] as const;

/** The `tl.so.tilo.remote.Connection` kind at `path`. */
export function connection(ws: tilo.Workspace, path: string): Connection {
  return resourceProxy(ws, path, SURFACE, CONNECTION_ACTIONS) as unknown as Connection;
}

/** Remote — `tl.so.tilo.remote.Remote`. */
export interface Remote extends tilo.Resource {
  yylg63a7: Array<tilo.Entry<Connection>>;  // connections
  /** Saves a machine to reach over SSH, VNC or as a Tilo device. */
  add_connection(args: {
    name: string;
    host: string;
    kind?: ConnectionKind;
    port?: number;
    user?: string;
    auth?: AuthMethod;
    group?: string;
  }): Promise<Connection>;
  /** Deletes a saved connection for everyone it is shared with; the machine itself is not changed. What this device kept to sign in to it goes too. */
  delete_connection(args: {
    c: Connection;
  }): Promise<void>;
  /** Runs `delete_connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_connectionMany(items: DeleteConnectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Saves the form: adds the connection or changes the one it was opened over, and keeps the password on this device. Answers whether it saved; a refusal leaves the form as it was. */
  save_form(args: {
    name: string;
    host: string;
    port: string;
    user: string;
    kind: ConnectionKind;
    auth: AuthMethod;
    group: string;
    secret: string;
    editing?: Connection;
  }): Promise<boolean>;
}

/** The actions `Remote` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REMOTE_ACTIONS = ["add_connection", "delete_connection", "delete_connectionMany", "save_form"] as const;

/** The `tl.so.tilo.remote.Remote` kind at `path`. */
export function remote(ws: tilo.Workspace, path: string): Remote {
  return resourceProxy(ws, path, SURFACE, REMOTE_ACTIONS) as unknown as Remote;
}

/** Session — `tl.so.tilo.remote.Session`. */
export interface Session extends tilo.Resource {
  fpphznkb: string;  // connection
  vvyt5lha: string;  // name
  amv62hzp: ConnectionKind;  // kind
  l3jskskl: string;  // job
  "3cerrtf7": string;  // terminal
  gazx3juc: string;  // trouble
  /** Ends a session; the saved connection stays. */
  close_session(): Promise<void>;
  /** Runs `close_session` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  close_sessionMany(items: CloseSessionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Notes what this device saw of the machine a session reached: up once it answered, down once it failed to. */
  observe(): Promise<void>;
  /** Runs `observe` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  observeMany(items: ObserveArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Session` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SESSION_ACTIONS = ["close_session", "close_sessionMany", "observe", "observeMany"] as const;

/** The `tl.so.tilo.remote.Session` kind at `path`. */
export function session(ws: tilo.Workspace, path: string): Session {
  return resourceProxy(ws, path, SURFACE, SESSION_ACTIONS) as unknown as Session;
}

/** Seen — `tl.so.tilo.remote.Seen`. */
export interface Seen extends tilo.Resource {
  rb2plwyy: string;  // connection
  hgy5dnvw: boolean;  // online
}

/** The actions `Seen` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SEEN_ACTIONS = [] as const;

/** The `tl.so.tilo.remote.Seen` kind at `path`. */
export function seen(ws: tilo.Workspace, path: string): Seen {
  return resourceProxy(ws, path, SURFACE, SEEN_ACTIONS) as unknown as Seen;
}

/** Secret — `tl.so.tilo.remote.Secret`. */
export interface Secret extends tilo.Resource {
  zfdaqp7c: string;  // connection
  fb6ane5u: string;  // purpose
  d6i33xnv: string;  // value
}

/** The actions `Secret` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SECRET_ACTIONS = [] as const;

/** The `tl.so.tilo.remote.Secret` kind at `path`. */
export function secret(ws: tilo.Workspace, path: string): Secret {
  return resourceProxy(ws, path, SURFACE, SECRET_ACTIONS) as unknown as Secret;
}

/** TrustedKey — `tl.so.tilo.remote.TrustedKey`. */
export interface TrustedKey extends tilo.Resource {
  rgmdf7xq: string;  // host
  cxui2vf5: number;  // port
  bmf56c7k: string;  // algorithm
  n3ikzj6y: string;  // fingerprint
}

/** The actions `TrustedKey` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TRUSTEDKEY_ACTIONS = [] as const;

/** The `tl.so.tilo.remote.TrustedKey` kind at `path`. */
export function trustedKey(ws: tilo.Workspace, path: string): TrustedKey {
  return resourceProxy(ws, path, SURFACE, TRUSTEDKEY_ACTIONS) as unknown as TrustedKey;
}

/** Here — `tl.so.tilo.remote.Here`. */
export interface Here extends tilo.Resource {
  mqn5woua: Array<tilo.Entry<Session>>;  // sessions
  b32nsjwp: Array<tilo.Entry<Seen>>;  // seen
  zdbfp7w6: Array<tilo.Entry<Secret>>;  // secrets
  jbjh2xut: Array<tilo.Entry<TrustedKey>>;  // trusted
}

/** The actions `Here` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HERE_ACTIONS = [] as const;

/** The `tl.so.tilo.remote.Here` kind at `path`. */
export function here(ws: tilo.Workspace, path: string): Here {
  return resourceProxy(ws, path, SURFACE, HERE_ACTIONS) as unknown as Here;
}

/** KeyId — `tl.so.tilo.remote.KeyId`. */
export interface KeyId extends tilo.Resource {
  x5gkgjv7: string;  // algorithm
  "5vpzlqkt": string;  // fingerprint
}

/** The actions `KeyId` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const KEYID_ACTIONS = [] as const;

/** The `tl.so.tilo.remote.KeyId` kind at `path`. */
export function keyId(ws: tilo.Workspace, path: string): KeyId {
  return resourceProxy(ws, path, SURFACE, KEYID_ACTIONS) as unknown as KeyId;
}

/** Said — `tl.so.tilo.remote.Said`. */
export interface Said extends tilo.Resource {
  azv5q3lb: string;  // phase
  app3bmk2: string;  // why
  "5owfkzza": string;  // algorithm
  unhnci5d: string;  // fingerprint
  gwkgueek: KeyId | null;  // expected
  pqs4atu7: KeyId | null;  // observed
}

/** The actions `Said` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAID_ACTIONS = [] as const;

/** The `tl.so.tilo.remote.Said` kind at `path`. */
export function said(ws: tilo.Workspace, path: string): Said {
  return resourceProxy(ws, path, SURFACE, SAID_ACTIONS) as unknown as Said;
}

/** SshAsk — `tl.so.tilo.remote.SshAsk`. */
export interface SshAsk extends tilo.Resource {
  qyt6kg5x: string;  // host
  hey2mljd: number;  // port
  lutwl6ee: string;  // user
  iiovxv6t: string;  // method
  u55jtdje: string | null;  // secret
  swuhzzts: KeyId | null;  // expected
  i4deiyeq: number;  // columns
  wrbw55me: number;  // rows
}

/** The actions `SshAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SSHASK_ACTIONS = [] as const;

/** The `tl.so.tilo.remote.SshAsk` kind at `path`. */
export function sshAsk(ws: tilo.Workspace, path: string): SshAsk {
  return resourceProxy(ws, path, SURFACE, SSHASK_ACTIONS) as unknown as SshAsk;
}

/** VncAsk — `tl.so.tilo.remote.VncAsk`. */
export interface VncAsk extends tilo.Resource {
  jtksl5yz: string;  // host
  ypo5oexy: number;  // port
  q3gwteu3: string | null;  // password
}

/** The actions `VncAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VNCASK_ACTIONS = [] as const;

/** The `tl.so.tilo.remote.VncAsk` kind at `path`. */
export function vncAsk(ws: tilo.Workspace, path: string): VncAsk {
  return resourceProxy(ws, path, SURFACE, VNCASK_ACTIONS) as unknown as VncAsk;
}

/** Started — `tl.so.tilo.remote.Started`. */
export interface Started extends tilo.Resource {
  hk6v4nxp: string;  // job
  k6qck3ko: string;  // session
}

/** The actions `Started` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STARTED_ACTIONS = [] as const;

/** The `tl.so.tilo.remote.Started` kind at `path`. */
export function started(ws: tilo.Workspace, path: string): Started {
  return resourceProxy(ws, path, SURFACE, STARTED_ACTIONS) as unknown as Started;
}

/** SendAsk — `tl.so.tilo.remote.SendAsk`. */
export interface SendAsk extends tilo.Resource {
  jlt5sla7: string;  // job
  "5kuradb3": string;  // text
}

/** The actions `SendAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SENDASK_ACTIONS = [] as const;

/** The `tl.so.tilo.remote.SendAsk` kind at `path`. */
export function sendAsk(ws: tilo.Workspace, path: string): SendAsk {
  return resourceProxy(ws, path, SURFACE, SENDASK_ACTIONS) as unknown as SendAsk;
}

/** TransferAsk — `tl.so.tilo.remote.TransferAsk`. */
export interface TransferAsk extends tilo.Resource {
  "376cxznk": string;  // job
  da2gioaf: string;  // local_path
  "33n5eruw": string;  // remote_path
}

/** The actions `TransferAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TRANSFERASK_ACTIONS = [] as const;

/** The `tl.so.tilo.remote.TransferAsk` kind at `path`. */
export function transferAsk(ws: tilo.Workspace, path: string): TransferAsk {
  return resourceProxy(ws, path, SURFACE, TRANSFERASK_ACTIONS) as unknown as TransferAsk;
}

/** Remote Desktop — `tl.so.tilo.remote`. */
export interface TiloRemote extends tilo.Resource {
}

/** The actions `TiloRemote` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOREMOTE_ACTIONS = [] as const;

/** The `tl.so.tilo.remote` kind at `path`. */
export function tiloRemote(ws: tilo.Workspace, path: string): TiloRemote {
  return resourceProxy(ws, path, SURFACE, TILOREMOTE_ACTIONS) as unknown as TiloRemote;
}

