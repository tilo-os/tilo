/**
 * Types for API Tester (`so.tilo.api-tester`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Row` is a `Resource` of the app's `tl.so.tilo.api-tester.Row`, made by its factory with the
 * resource's path (`api-tester/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.api-tester@e5a0455b27ec3206";

/** How a request signs in. One of "none", "bearer", "basic", "api_key". */
export type AuthScheme = "none" | "bearer" | "basic" | "api_key";

/** What a request's body is. One of "none", "json", "form", "raw". */
export type BodyMode = "none" | "json" | "form" | "raw";

/** The methods a request is sent with, in the order the chooser lists them. One of "get", "post", "put", "patch", "delete", "head", "options". */
export type HttpMethod = "get" | "post" | "put" | "patch" | "delete" | "head" | "options";

/** The arguments of `fill_from`. */
export interface FillFromArgs {
  src: Request;
}

/** The arguments of `add_param`. */
export interface AddParamArgs {
}

/** The arguments of `delete_param`. */
export interface DeleteParamArgs {
  entry: Row;
}

/** The arguments of `add_header`. */
export interface AddHeaderArgs {
}

/** The arguments of `delete_header`. */
export interface DeleteHeaderArgs {
  entry: Row;
}

/** The arguments of `add_form_field`. */
export interface AddFormFieldArgs {
}

/** The arguments of `delete_form_field`. */
export interface DeleteFormFieldArgs {
  entry: Row;
}

/** The arguments of `rename_environment`. */
export interface RenameEnvironmentArgs {
  name: string;
}

/** The arguments of `delete_variable`. */
export interface DeleteVariableArgs {
  v: Variable;
}

/** The arguments of `keep_variable`. */
export interface KeepVariableArgs {
  value: string;
}

/** The arguments of `rename_collection`. */
export interface RenameCollectionArgs {
  name: string;
}

/** The arguments of `delete_collection`. */
export interface DeleteCollectionArgs {
  c: SavedCollection;
}

/** The arguments of `open_request`. */
export interface OpenRequestArgs {
  r: Request;
}

/** The arguments of `rename_draft`. */
export interface RenameDraftArgs {
  name: string;
}

/** The arguments of `delete_request`. */
export interface DeleteRequestArgs {
  r: Request;
}

/** The arguments of `delete_opened_request`. */
export interface DeleteOpenedRequestArgs {
}

/** The arguments of `delete_environment`. */
export interface DeleteEnvironmentArgs {
  e: Environment;
}

/** The arguments of `delete_active_environment`. */
export interface DeleteActiveEnvironmentArgs {
}

/** The arguments of `use_environment`. */
export interface UseEnvironmentArgs {
  e: Environment;
}

/** The arguments of `no_environment`. */
export interface NoEnvironmentArgs {
}

/** The arguments of `keep_credential`. */
export interface KeepCredentialArgs {
  value: string;
}

/** The arguments of `set_scheme`. */
export interface SetSchemeArgs {
  scheme: AuthScheme;
}

/** The arguments of `forget_credential`. */
export interface ForgetCredentialArgs {
}

/** The arguments of `send`. */
export interface SendArgs {
}

/** The arguments of `cancel_send`. */
export interface CancelSendArgs {
}

/** The arguments of `poll_send`. */
export interface PollSendArgs {
}

/** The arguments of `open_sent`. */
export interface OpenSentArgs {
  s: Sent;
}

/** The arguments of `delete_sent`. */
export interface DeleteSentArgs {
  s: Sent;
}

/** The arguments of `dismiss_notice`. */
export interface DismissNoticeArgs {
}

/** The arguments of `keep_answer`. */
export interface KeepAnswerArgs {
  answer: string;
}

/** The arguments of `record_sent`. */
export interface RecordSentArgs {
  answer: string;
}

/** The arguments of `set_body_mode`. */
export interface SetBodyModeArgs {
  mode: BodyMode;
}

/** Row — `tl.so.tilo.api-tester.Row`. */
export interface Row extends tilo.Resource {
  gmr3auev: string;  // key
  "5i27urp3": string;  // value
  vgdlc7cu: boolean;  // enabled
}

/** The actions `Row` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ROW_ACTIONS = [] as const;

/** The `tl.so.tilo.api-tester.Row` kind at `path`. */
export function row(ws: tilo.Workspace, path: string): Row {
  return resourceProxy(ws, path, SURFACE, ROW_ACTIONS) as unknown as Row;
}

/** Credentials — `tl.so.tilo.api-tester.Credentials`. */
export interface Credentials extends tilo.Resource {
  wu5jqin3: AuthScheme;  // scheme
  eimiw54w: string;  // username
  bq35bust: string;  // header
  doie7hps: string;  // reference
}

/** The actions `Credentials` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CREDENTIALS_ACTIONS = [] as const;

/** The `tl.so.tilo.api-tester.Credentials` kind at `path`. */
export function credentials(ws: tilo.Workspace, path: string): Credentials {
  return resourceProxy(ws, path, SURFACE, CREDENTIALS_ACTIONS) as unknown as Credentials;
}

/** Request — `tl.so.tilo.api-tester.Request`. */
export interface Request extends tilo.Resource {
  zerzgb7o: string;  // name
  plszv7l4: HttpMethod;  // method
  "2myyqmpp": string;  // url
  d26dsrkb: Array<tilo.Entry<Row>>;  // params
  sp2vfioq: Array<tilo.Entry<Row>>;  // headers
  oubruxyi: Credentials;  // auth
  "26dsckeq": BodyMode;  // body_mode
  ejporjru: string;  // body
  "2koo3omi": Array<tilo.Entry<Row>>;  // form
  /** Copies everything a request holds into another one. */
  fill_from(args: {
    src: Request;
  }): Promise<void>;
  /** Runs `fill_from` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  fill_fromMany(items: FillFromArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a query parameter to the request in the builder. */
  add_param(): Promise<void>;
  /** Runs `add_param` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_paramMany(items: AddParamArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a query parameter off the request in the builder. */
  delete_param(args: {
    entry: Row;
  }): Promise<void>;
  /** Runs `delete_param` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_paramMany(items: DeleteParamArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a header to the request in the builder. */
  add_header(): Promise<void>;
  /** Runs `add_header` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_headerMany(items: AddHeaderArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a header off the request in the builder. */
  delete_header(args: {
    entry: Row;
  }): Promise<void>;
  /** Runs `delete_header` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_headerMany(items: DeleteHeaderArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a form field to the request in the builder. */
  add_form_field(): Promise<void>;
  /** Runs `add_form_field` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_form_fieldMany(items: AddFormFieldArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a form field off the request in the builder. */
  delete_form_field(args: {
    entry: Row;
  }): Promise<void>;
  /** Runs `delete_form_field` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_form_fieldMany(items: DeleteFormFieldArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Request` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REQUEST_ACTIONS = ["fill_from", "fill_fromMany", "add_param", "add_paramMany", "delete_param", "delete_paramMany", "add_header", "add_headerMany", "delete_header", "delete_headerMany", "add_form_field", "add_form_fieldMany", "delete_form_field", "delete_form_fieldMany"] as const;

/** The `tl.so.tilo.api-tester.Request` kind at `path`. */
export function request(ws: tilo.Workspace, path: string): Request {
  return resourceProxy(ws, path, SURFACE, REQUEST_ACTIONS) as unknown as Request;
}

/** Answer — `tl.so.tilo.api-tester.Answer`. */
export interface Answer extends tilo.Resource {
  cmtsv52e: number;  // status
  s2rpes4w: string;  // status_text
  bmbte2qq: number;  // duration_ms
  "7xxkbrmd": number;  // size_bytes
  lfculcqs: boolean;  // size_exact
  "52brcezl": number;  // shown_bytes
  dczl7ahz: boolean;  // binary
  ue25vzen: string;  // content_type
  nxs5lmpc: string;  // body
  ryqihqco: string;  // pretty
  anvyklqi: Array<tilo.Entry<AnswerHeader>>;  // headers
}

/** The actions `Answer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.api-tester.Answer` kind at `path`. */
export function answer(ws: tilo.Workspace, path: string): Answer {
  return resourceProxy(ws, path, SURFACE, ANSWER_ACTIONS) as unknown as Answer;
}

/** AnswerHeader — `tl.so.tilo.api-tester.AnswerHeader`. */
export interface AnswerHeader extends tilo.Resource {
  q5ydbtzd: string;  // name
  vyjpofsi: string;  // value
}

/** The actions `AnswerHeader` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ANSWERHEADER_ACTIONS = [] as const;

/** The `tl.so.tilo.api-tester.AnswerHeader` kind at `path`. */
export function answerHeader(ws: tilo.Workspace, path: string): AnswerHeader {
  return resourceProxy(ws, path, SURFACE, ANSWERHEADER_ACTIONS) as unknown as AnswerHeader;
}

/** Environment — `tl.so.tilo.api-tester.Environment`. */
export interface Environment extends tilo.Resource {
  "2kunn2pf": string;  // name
  yhhzdhos: Array<tilo.Entry<Variable>>;  // variables
  /** Renames an environment. */
  rename_environment(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename_environment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_environmentMany(items: RenameEnvironmentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a variable to an environment. A secret's value is typed after it stands. */
  add_variable(args: {
    key: string;
    secret?: boolean;
  }): Promise<Variable>;
  /** Takes a variable off its environment, and its value off this device. */
  delete_variable(args: {
    v: Variable;
  }): Promise<void>;
  /** Runs `delete_variable` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_variableMany(items: DeleteVariableArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Environment` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ENVIRONMENT_ACTIONS = ["rename_environment", "rename_environmentMany", "add_variable", "delete_variable", "delete_variableMany"] as const;

/** The `tl.so.tilo.api-tester.Environment` kind at `path`. */
export function environment(ws: tilo.Workspace, path: string): Environment {
  return resourceProxy(ws, path, SURFACE, ENVIRONMENT_ACTIONS) as unknown as Environment;
}

/** Variable — `tl.so.tilo.api-tester.Variable`. */
export interface Variable extends tilo.Resource {
  i3e2wno7: string;  // key
  qqgvzyfe: string;  // value
  "7bxonldr": boolean;  // enabled
  og7vyhyy: boolean;  // secret
  v6jf7adc: string;  // reference
  /** Keeps the value typed for a secret variable where the workspace cannot read it. */
  keep_variable(args: {
    value: string;
  }): Promise<void>;
  /** Runs `keep_variable` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_variableMany(items: KeepVariableArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Variable` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VARIABLE_ACTIONS = ["keep_variable", "keep_variableMany"] as const;

/** The `tl.so.tilo.api-tester.Variable` kind at `path`. */
export function variable(ws: tilo.Workspace, path: string): Variable {
  return resourceProxy(ws, path, SURFACE, VARIABLE_ACTIONS) as unknown as Variable;
}

/** Sent — `tl.so.tilo.api-tester.Sent`. */
export interface Sent extends tilo.Resource {
  ikluhyoi: string;  // route
  fwwi364f: HttpMethod;  // method
  w7aoohfv: string;  // url
  "7pfct73j": number;  // status
  cno6gxwc: number;  // duration_ms
  irpsjk53: string;  // at
  "4mnpilrd": string;  // replay
}

/** The actions `Sent` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SENT_ACTIONS = [] as const;

/** The `tl.so.tilo.api-tester.Sent` kind at `path`. */
export function sent(ws: tilo.Workspace, path: string): Sent {
  return resourceProxy(ws, path, SURFACE, SENT_ACTIONS) as unknown as Sent;
}

/** SavedCollection — `tl.so.tilo.api-tester.SavedCollection`. */
export interface SavedCollection extends tilo.Resource {
  "6bygepf4": string;  // name
  vghsgjnl: Array<tilo.Entry<Request>>;  // requests
  /** Renames a collection. */
  rename_collection(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename_collection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_collectionMany(items: RenameCollectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a request to a collection. */
  add_request(args?: {
    name?: string;
  }): Promise<Request>;
}

/** The actions `SavedCollection` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVEDCOLLECTION_ACTIONS = ["rename_collection", "rename_collectionMany", "add_request"] as const;

/** The `tl.so.tilo.api-tester.SavedCollection` kind at `path`. */
export function savedCollection(ws: tilo.Workspace, path: string): SavedCollection {
  return resourceProxy(ws, path, SURFACE, SAVEDCOLLECTION_ACTIONS) as unknown as SavedCollection;
}

/** Workspace — `tl.so.tilo.api-tester.Workspace`. */
export interface Workspace extends tilo.Resource {
  bqk5mejh: Array<tilo.Entry<SavedCollection>>;  // collections
  "7idvazp5": Array<tilo.Entry<Environment>>;  // environments
  bnuffmnw: Environment | null;  // active_environment
  t7bbwzg4: Request;  // builder
  oela3gry: Request | null;  // opened
  yughiaxm: Array<tilo.Entry<Sent>>;  // history
  ylmn6amm: boolean;  // sending
  lzzl5iuz: string;  // job
  be7aufil: string;  // sent_replay
  "6msktvdz": Answer;  // answer
  bhgazpm3: boolean;  // answered
  "674n5s6g": string;  // failure
  si6y5pgr: string;  // notice
  /** Starts a blank request in the builder, leaving the one it held. */
  new_request(): Promise<Request>;
  /** Creates a collection to keep requests in. */
  new_collection(args: {
    name: string;
  }): Promise<SavedCollection>;
  /** Deletes a collection and the requests saved in it. */
  delete_collection(args: {
    c: SavedCollection;
  }): Promise<void>;
  /** Runs `delete_collection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_collectionMany(items: DeleteCollectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Saves the request in the builder into a collection. */
  save_request(args: {
    c: SavedCollection;
  }): Promise<Request>;
  /** Opens a saved request in the builder. */
  open_request(args: {
    r: Request;
  }): Promise<void>;
  /** Runs `open_request` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_requestMany(items: OpenRequestArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames the request in the builder. */
  rename_draft(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename_draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_draftMany(items: RenameDraftArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a saved request. */
  delete_request(args: {
    r: Request;
  }): Promise<void>;
  /** Runs `delete_request` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_requestMany(items: DeleteRequestArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes the saved request the builder holds a copy of. */
  delete_opened_request(): Promise<void>;
  /** Runs `delete_opened_request` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_opened_requestMany(items: DeleteOpenedRequestArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Creates an environment and starts using it. */
  new_environment(args: {
    name: string;
  }): Promise<Environment>;
  /** Deletes an environment and the variables it held. */
  delete_environment(args: {
    e: Environment;
  }): Promise<void>;
  /** Runs `delete_environment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_environmentMany(items: DeleteEnvironmentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes the environment in use. */
  delete_active_environment(): Promise<void>;
  /** Runs `delete_active_environment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_active_environmentMany(items: DeleteActiveEnvironmentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Switches the environment whose variables fill what is sent. */
  use_environment(args: {
    e: Environment;
  }): Promise<void>;
  /** Runs `use_environment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  use_environmentMany(items: UseEnvironmentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sends with no environment: every `{{variable}}` stays as typed. */
  no_environment(): Promise<void>;
  /** Runs `no_environment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  no_environmentMany(items: NoEnvironmentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps the value typed for the request's sign-in where the workspace cannot read it, and remembers only the reference. */
  keep_credential(args: {
    value: string;
  }): Promise<void>;
  /** Runs `keep_credential` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_credentialMany(items: KeepCredentialArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Changes the scheme, taking the credential of the one before off this device. */
  set_scheme(args: {
    scheme: AuthScheme;
  }): Promise<void>;
  /** Runs `set_scheme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_schemeMany(items: SetSchemeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the request's sign-in off this device. */
  forget_credential(): Promise<void>;
  /** Runs `forget_credential` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forget_credentialMany(items: ForgetCredentialArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sends what the builder holds to the address it names, in the environment in use. */
  send(): Promise<void>;
  /** Runs `send` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sendMany(items: SendArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops waiting for the service to answer. */
  cancel_send(): Promise<void>;
  /** Runs `cancel_send` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancel_sendMany(items: CancelSendArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the device how the request on its way is doing, and keeps what it answered. */
  poll_send(): Promise<void>;
  /** Runs `poll_send` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  poll_sendMany(items: PollSendArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens a request that was sent before, with what it held when it went out. */
  open_sent(args: {
    s: Sent;
  }): Promise<void>;
  /** Runs `open_sent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_sentMany(items: OpenSentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Saves a sent request as a request in a collection, named by its route unless it had a name. */
  save_sent(args: {
    s: Sent;
    c: SavedCollection;
  }): Promise<Request>;
  /** Forgets a sent request. */
  delete_sent(args: {
    s: Sent;
  }): Promise<void>;
  /** Runs `delete_sent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_sentMany(items: DeleteSentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies the response body, exactly as the response pane shows it. */
  copy_body(): Promise<void>;
  /** Copies every response header, one `Name: value` per line. */
  copy_headers(): Promise<void>;
  /** Copies the request as authored, with its `{{variables}}` and never a credential. */
  copy_request(): Promise<void>;
  /** Dismisses the line under the response. */
  dismiss_notice(): Promise<void>;
  /** Runs `dismiss_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismiss_noticeMany(items: DismissNoticeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps the answer in the workspace, so the window and an agent read the same one. */
  keep_answer(args: {
    answer: string;
  }): Promise<void>;
  /** Runs `keep_answer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_answerMany(items: KeepAnswerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds the request that just came back to the workspace's history, newest first. */
  record_sent(args: {
    answer: string;
  }): Promise<void>;
  /** Runs `record_sent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  record_sentMany(items: RecordSentArgs[]): Promise<tilo.ActionAnswer[]>;
  set_body_mode(args: {
    mode: BodyMode;
  }): Promise<void>;
  /** Runs `set_body_mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_body_modeMany(items: SetBodyModeArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Workspace` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKSPACE_ACTIONS = ["new_request", "new_collection", "delete_collection", "delete_collectionMany", "save_request", "open_request", "open_requestMany", "rename_draft", "rename_draftMany", "delete_request", "delete_requestMany", "delete_opened_request", "delete_opened_requestMany", "new_environment", "delete_environment", "delete_environmentMany", "delete_active_environment", "delete_active_environmentMany", "use_environment", "use_environmentMany", "no_environment", "no_environmentMany", "keep_credential", "keep_credentialMany", "set_scheme", "set_schemeMany", "forget_credential", "forget_credentialMany", "send", "sendMany", "cancel_send", "cancel_sendMany", "poll_send", "poll_sendMany", "open_sent", "open_sentMany", "save_sent", "delete_sent", "delete_sentMany", "copy_body", "copy_headers", "copy_request", "dismiss_notice", "dismiss_noticeMany", "keep_answer", "keep_answerMany", "record_sent", "record_sentMany", "set_body_mode", "set_body_modeMany"] as const;

/** The `tl.so.tilo.api-tester.Workspace` kind at `path`. */
export function workspace(ws: tilo.Workspace, path: string): Workspace {
  return resourceProxy(ws, path, SURFACE, WORKSPACE_ACTIONS) as unknown as Workspace;
}

/** SendAsk — `tl.so.tilo.api-tester.SendAsk`. */
export interface SendAsk extends tilo.Resource {
  natoiy5b: string;  // method
  xg26wm2t: string;  // url
  "52ykdq3w": string[][];  // params
  "2g54j6fl": string[][];  // headers
  "5hvbl66x": string[][];  // vars
  o4mypkv3: string;  // auth_scheme
  wgtleqed: string;  // auth_username
  "5wzmu546": string;  // auth_header
  "6wq4j3vq": string;  // auth_reference
  dnf4fjhy: string;  // body_mode
  um3yzxeu: string;  // body
  m7c3owzv: string[][];  // form
}

/** The actions `SendAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SENDASK_ACTIONS = [] as const;

/** The `tl.so.tilo.api-tester.SendAsk` kind at `path`. */
export function sendAsk(ws: tilo.Workspace, path: string): SendAsk {
  return resourceProxy(ws, path, SURFACE, SENDASK_ACTIONS) as unknown as SendAsk;
}

/** KeepAsk — `tl.so.tilo.api-tester.KeepAsk`. */
export interface KeepAsk extends tilo.Resource {
  qxcspakn: string;  // value
}

/** The actions `KeepAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const KEEPASK_ACTIONS = [] as const;

/** The `tl.so.tilo.api-tester.KeepAsk` kind at `path`. */
export function keepAsk(ws: tilo.Workspace, path: string): KeepAsk {
  return resourceProxy(ws, path, SURFACE, KEEPASK_ACTIONS) as unknown as KeepAsk;
}

/** ForgetAsk — `tl.so.tilo.api-tester.ForgetAsk`. */
export interface ForgetAsk extends tilo.Resource {
  "42ltyzyi": string;  // reference
}

/** The actions `ForgetAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FORGETASK_ACTIONS = [] as const;

/** The `tl.so.tilo.api-tester.ForgetAsk` kind at `path`. */
export function forgetAsk(ws: tilo.Workspace, path: string): ForgetAsk {
  return resourceProxy(ws, path, SURFACE, FORGETASK_ACTIONS) as unknown as ForgetAsk;
}

/** KeptCredential — `tl.so.tilo.api-tester.KeptCredential`. */
export interface KeptCredential extends tilo.Resource {
  haaxlsd4: string;  // reference
}

/** The actions `KeptCredential` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const KEPTCREDENTIAL_ACTIONS = [] as const;

/** The `tl.so.tilo.api-tester.KeptCredential` kind at `path`. */
export function keptCredential(ws: tilo.Workspace, path: string): KeptCredential {
  return resourceProxy(ws, path, SURFACE, KEPTCREDENTIAL_ACTIONS) as unknown as KeptCredential;
}

/** SendState — `tl.so.tilo.api-tester.SendState`. */
export interface SendState extends tilo.Resource {
  tgafs5zj: string;  // state
  vvonqua3: string;  // answer
  or6k3lr7: string;  // error
}

/** The actions `SendState` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SENDSTATE_ACTIONS = [] as const;

/** The `tl.so.tilo.api-tester.SendState` kind at `path`. */
export function sendState(ws: tilo.Workspace, path: string): SendState {
  return resourceProxy(ws, path, SURFACE, SENDSTATE_ACTIONS) as unknown as SendState;
}

/** Answered — `tl.so.tilo.api-tester.Answered`. */
export interface Answered extends tilo.Resource {
  s4gqhsru: number;  // status
  kqbtxrg4: string;  // status_text
  "3ffoqs7c": number;  // duration_ms
  aciatrpq: number;  // size_bytes
  yjlhfc23: boolean;  // size_exact
  n36ppxll: number;  // shown_bytes
  f4fheroi: boolean;  // binary
  loetbxur: string;  // content_type
  "5c64ziqe": string;  // body
  agnxdiuh: string[][];  // headers
}

/** The actions `Answered` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ANSWERED_ACTIONS = [] as const;

/** The `tl.so.tilo.api-tester.Answered` kind at `path`. */
export function answered(ws: tilo.Workspace, path: string): Answered {
  return resourceProxy(ws, path, SURFACE, ANSWERED_ACTIONS) as unknown as Answered;
}

/** Replay — `tl.so.tilo.api-tester.Replay`. */
export interface Replay extends tilo.Resource {
  "7fy3fg22": string;  // name
  woxdrlbr: string;  // method
  uehndmlh: string;  // url
  dw7wgcqy: string[][];  // params
  oxqptjsf: string[][];  // headers
  i66no2tx: string[][];  // form
  c7sufjee: string;  // scheme
  c4lbngho: string;  // username
  p3sqgmt6: string;  // header
  wczlgfhd: string;  // reference
  x5jj7sdv: string;  // body_mode
  "5ez7t24e": string;  // body
}

/** The actions `Replay` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPLAY_ACTIONS = [] as const;

/** The `tl.so.tilo.api-tester.Replay` kind at `path`. */
export function replay(ws: tilo.Workspace, path: string): Replay {
  return resourceProxy(ws, path, SURFACE, REPLAY_ACTIONS) as unknown as Replay;
}

/** API Tester — `tl.so.tilo.api-tester`. */
export interface ApiTester extends tilo.Resource {
  /** Takes a stored value off this device. */
  forget_value(args: {
    reference: string;
  }): Promise<void>;
}

/** The actions `ApiTester` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const APITESTER_ACTIONS = ["forget_value"] as const;

/** The `tl.so.tilo.api-tester` kind at `path`. */
export function apiTester(ws: tilo.Workspace, path: string): ApiTester {
  return resourceProxy(ws, path, SURFACE, APITESTER_ACTIONS) as unknown as ApiTester;
}

