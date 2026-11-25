/**
 * Types for Shortcuts (`so.tilo.shortcuts`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`shortcuts/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.shortcuts@dd32ad37ca4641ea";

/** How a value is held: typed, one of the shortcut's inputs, a variable, an earlier step's output, text with placeholders, or an object or list of values. One of "none", "literal", "input", "variable", "step", "template", "object", "list". */
export type Bind = "none" | "literal" | "input" | "variable" | "step" | "template" | "object" | "list";

/** What a branch asks. One of "truthy", "equals", "negate", "all", "any". */
export type Cond = "truthy" | "equals" | "negate" | "all" | "any";

/** How a value a person chose is kept: not chosen, one of the values in scope, or a value typed. One of "none", "choice", "literal". */
export type Pick = "none" | "choice" | "literal";

/** Which block of a step the step inside it runs in. One of "top", "then", "otherwise", "each". */
export type Branch = "top" | "then" | "otherwise" | "each";

/** What a step does. One of "invoke", "branch", "each", "set", "ask", "ai", "task", "read". */
export type Head = "invoke" | "branch" | "each" | "set" | "ask" | "ai" | "task" | "read";

/** The kinds of value a shortcut takes when it is started. One of "text", "number", "integer", "boolean", "date_time", "resource". */
export type InputType = "text" | "number" | "integer" | "boolean" | "date_time" | "resource";

/** What a run does when a step fails. One of "fail", "retry", "fallback", "carry_on". */
export type Policy = "fail" | "retry" | "fallback" | "carry_on";

/** How a step names the resource its action is called on. One of "fixed", "kind", "binding". */
export type Selector = "fixed" | "kind" | "binding";

/** Whose changes a record trigger counts. One of "anyone", "people", "not_bots". */
export type By = "anyone" | "people" | "not_bots";

/** Which network changes a network trigger counts. One of "any", "connected", "disconnected". */
export type Connected = "any" | "connected" | "disconnected";

/** What a focus-mode trigger watches: Do Not Disturb or the focus schedule. One of "do_not_disturb", "focus". */
export type FocusMode = "do_not_disturb" | "focus";

/** Whether it watches the mode switching on or off. One of "enter", "exit". */
export type FocusTransition = "enter" | "exit";

/** What starts a saved shortcut on its own. One of "keyboard", "app_opened", "schedule_day", "schedule_every", "once", "workspace_joined", "file_changed", "focus_mode", "network_changed", "device_connected", "resource_changed", "app_event", "incoming_message", "nearby_device", "resource_time", "source_changed". */
export type TriggerKind = "keyboard" | "app_opened" | "schedule_day" | "schedule_every" | "once" | "workspace_joined" | "file_changed" | "focus_mode" | "network_changed" | "device_connected" | "resource_changed" | "app_event" | "incoming_message" | "nearby_device" | "resource_time" | "source_changed";

/** The arguments of `load_value`. */
export interface LoadValueArgs {
  value: unknown;
}

/** The arguments of `adopt_choice`. */
export interface AdoptChoiceArgs {
  choice: Choice;
}

/** The arguments of `copy_value`. */
export interface CopyValueArgs {
  from: BindValue;
}

/** The arguments of `load_condition`. */
export interface LoadConditionArgs {
  value: unknown;
}

/** The arguments of `copy_condition`. */
export interface CopyConditionArgs {
  from: Condition;
}

/** The arguments of `load_choice`. */
export interface LoadChoiceArgs {
  value: unknown;
}

/** The arguments of `load_arg`. */
export interface LoadArgArgs {
  name: string;
  value: unknown;
}

/** The arguments of `apply_offer`. */
export interface ApplyOfferArgs {
  offered: unknown;
}

/** The arguments of `apply_trigger`. */
export interface ApplyTriggerArgs {
  from?: Trigger;
}

/** The arguments of `load_slot`. */
export interface LoadSlotArgs {
  s: Step;
  name: string;
  value: unknown;
}

/** The arguments of `load_program`. */
export interface LoadProgramArgs {
  item: unknown;
}

/** The arguments of `load_block`. */
export interface LoadBlockArgs {
  branch: Branch;
  steps?: unknown;
  parent?: Step;
}

/** The arguments of `load_step`. */
export interface LoadStepArgs {
  item: unknown;
  branch: Branch;
  parent?: Step;
}

/** The arguments of `load_triggers`. */
export interface LoadTriggersArgs {
  triggers: unknown[];
}

/** The arguments of `load_saved`. */
export interface LoadSavedArgs {
  value: unknown;
}

/** The arguments of `load_shortcut`. */
export interface LoadShortcutArgs {
  program: unknown;
}

/** The arguments of `copy_block`. */
export interface CopyBlockArgs {
  to: Draft;
  branch: Branch;
  parent?: Step;
}

/** The arguments of `disarm`. */
export interface DisarmArgs {
}

/** The arguments of `remove_below`. */
export interface RemoveBelowArgs {
  s: Step;
}

/** The arguments of `match_catalogue`. */
export interface MatchCatalogueArgs {
}

/** The arguments of `new_draft`. */
export interface NewDraftArgs {
  name?: string;
}

/** The arguments of `discard`. */
export interface DiscardArgs {
  d: Draft;
}

/** The arguments of `ignore_proposal`. */
export interface IgnoreProposalArgs {
  key: string;
}

/** The arguments of `allow_proposal`. */
export interface AllowProposalArgs {
  key: string;
}

/** The arguments of `open_picker`. */
export interface OpenPickerArgs {
  d: Draft;
  block: string;
}

/** The arguments of `close_picker`. */
export interface ClosePickerArgs {
}

/** The arguments of `edit_trigger`. */
export interface EditTriggerArgs {
  t: Trigger;
}

/** The arguments of `cancel_edit`. */
export interface CancelEditArgs {
}

/** The arguments of `dismiss_suggested`. */
export interface DismissSuggestedArgs {
  s: Suggested;
}

/** The arguments of `toggle_fold`. */
export interface ToggleFoldArgs {
  d: Draft;
  path: string;
}

/** The arguments of `show_sheet`. */
export interface ShowSheetArgs {
  sheet: string;
}

/** The arguments of `clear_new_input`. */
export interface ClearNewInputArgs {
}

/** The arguments of `toggle_day`. */
export interface ToggleDayArgs {
  day: string;
}

/** The arguments of `dismiss_notice`. */
export interface DismissNoticeArgs {
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

/** Param — `tl.so.tilo.shortcuts.Param`. */
export interface Param extends tilo.Resource {
  w7j2mzpu: string;  // name
  ckvfwyfc: string;  // ty
  lovculcl: boolean;  // required
  dley6sq2: string;  // description
}

/** The actions `Param` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PARAM_ACTIONS = [] as const;

/** The `tl.so.tilo.shortcuts.Param` kind at `path`. */
export function param(ws: tilo.Workspace, path: string): Param {
  return resourceProxy(ws, path, SURFACE, PARAM_ACTIONS) as unknown as Param;
}

/** Member — `tl.so.tilo.shortcuts.Member`. */
export interface Member extends tilo.Resource {
  mvlmdz3t: string;  // at
  orfu2qci: BindValue;  // value
}

/** The actions `Member` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MEMBER_ACTIONS = [] as const;

/** The `tl.so.tilo.shortcuts.Member` kind at `path`. */
export function member(ws: tilo.Workspace, path: string): Member {
  return resourceProxy(ws, path, SURFACE, MEMBER_ACTIONS) as unknown as Member;
}

/** BindValue — `tl.so.tilo.shortcuts.BindValue`. */
export interface BindValue extends tilo.Resource {
  a43zwzxm: Bind;  // kind
  nu47tafs: string;  // ty
  q5rbcvgr: string;  // name
  dzmxkvwn: string;  // pointer
  bktowd37: string;  // literal
  iu7blqby: Array<tilo.Entry<Member>>;  // members
  /** Reads a value the engine wrote into `into`, and what an object or list holds inside it. */
  load_value(args: {
    value: unknown;
  }): Promise<void>;
  /** Runs `load_value` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_valueMany(items: LoadValueArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies a value the checker offered into an argument's or a slot's own value. */
  adopt_choice(args: {
    choice: Choice;
  }): Promise<void>;
  /** Runs `adopt_choice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  adopt_choiceMany(items: AdoptChoiceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies `from` into `into`, with what an object or a list holds. */
  copy_value(args: {
    from: BindValue;
  }): Promise<void>;
  /** Runs `copy_value` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  copy_valueMany(items: CopyValueArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `BindValue` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BINDVALUE_ACTIONS = ["load_value", "load_valueMany", "adopt_choice", "adopt_choiceMany", "copy_value", "copy_valueMany"] as const;

/** The `tl.so.tilo.shortcuts.BindValue` kind at `path`. */
export function bindValue(ws: tilo.Workspace, path: string): BindValue {
  return resourceProxy(ws, path, SURFACE, BINDVALUE_ACTIONS) as unknown as BindValue;
}

/** Condition — `tl.so.tilo.shortcuts.Condition`. */
export interface Condition extends tilo.Resource {
  "5qdigjgy": Cond;  // kind
  jm3nn7p2: BindValue;  // left
  o4jmv2xp: BindValue;  // right
  lwqjd6de: Array<tilo.Entry<Condition>>;  // inner
  /** Reads a condition the engine wrote into `c`. */
  load_condition(args: {
    value: unknown;
  }): Promise<void>;
  /** Runs `load_condition` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_conditionMany(items: LoadConditionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies a condition into `into`, with the conditions it joins. */
  copy_condition(args: {
    from: Condition;
  }): Promise<void>;
  /** Runs `copy_condition` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  copy_conditionMany(items: CopyConditionArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Condition` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONDITION_ACTIONS = ["load_condition", "load_conditionMany", "copy_condition", "copy_conditionMany"] as const;

/** The `tl.so.tilo.shortcuts.Condition` kind at `path`. */
export function condition(ws: tilo.Workspace, path: string): Condition {
  return resourceProxy(ws, path, SURFACE, CONDITION_ACTIONS) as unknown as Condition;
}

/** Choice — `tl.so.tilo.shortcuts.Choice`. */
export interface Choice extends tilo.Resource {
  jjnl5c5u: Bind;  // kind
  mf4qff6c: string;  // ty
  kufc2lcn: string;  // name
  "3uy3tubv": string;  // pointer
  gouakd6l: string;  // literal
  bi3zwtrn: string;  // describes
  /** Reads one value the checker offered into `choice`, whose parts are the value's own. */
  load_choice(args: {
    value: unknown;
  }): Promise<void>;
  /** Runs `load_choice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_choiceMany(items: LoadChoiceArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Choice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHOICE_ACTIONS = ["load_choice", "load_choiceMany"] as const;

/** The `tl.so.tilo.shortcuts.Choice` kind at `path`. */
export function choice(ws: tilo.Workspace, path: string): Choice {
  return resourceProxy(ws, path, SURFACE, CHOICE_ACTIONS) as unknown as Choice;
}

/** Arg — `tl.so.tilo.shortcuts.Arg`. */
export interface Arg extends tilo.Resource {
  jrmg23bl: string;  // name
  "6cqaypb4": string;  // ty
  dqswtaus: Pick;  // pick
  bqmee32h: BindValue;  // value
  nq3xfb3y: Array<tilo.Entry<Choice>>;  // choices
}

/** The actions `Arg` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ARG_ACTIONS = [] as const;

/** The `tl.so.tilo.shortcuts.Arg` kind at `path`. */
export function arg(ws: tilo.Workspace, path: string): Arg {
  return resourceProxy(ws, path, SURFACE, ARG_ACTIONS) as unknown as Arg;
}

/** Slot — `tl.so.tilo.shortcuts.Slot`. */
export interface Slot extends tilo.Resource {
  "6l2kcrfo": string;  // name
  bwqrb4mc: string;  // ty
  ns6t6yay: Pick;  // pick
  a5ejwysp: BindValue;  // value
}

/** The actions `Slot` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SLOT_ACTIONS = [] as const;

/** The `tl.so.tilo.shortcuts.Slot` kind at `path`. */
export function slot(ws: tilo.Workspace, path: string): Slot {
  return resourceProxy(ws, path, SURFACE, SLOT_ACTIONS) as unknown as Slot;
}

/** Problem — `tl.so.tilo.shortcuts.Problem`. */
export interface Problem extends tilo.Resource {
  ddjwwbo3: string;  // path
  yyjdqeh7: string;  // message
}

/** The actions `Problem` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROBLEM_ACTIONS = [] as const;

/** The `tl.so.tilo.shortcuts.Problem` kind at `path`. */
export function problem(ws: tilo.Workspace, path: string): Problem {
  return resourceProxy(ws, path, SURFACE, PROBLEM_ACTIONS) as unknown as Problem;
}

/** Grant — `tl.so.tilo.shortcuts.Grant`. */
export interface Grant extends tilo.Resource {
  "44e6m2wx": string;  // label
  w6pqc4i2: string;  // kind
}

/** The actions `Grant` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GRANT_ACTIONS = [] as const;

/** The `tl.so.tilo.shortcuts.Grant` kind at `path`. */
export function grant(ws: tilo.Workspace, path: string): Grant {
  return resourceProxy(ws, path, SURFACE, GRANT_ACTIONS) as unknown as Grant;
}

/** Step — `tl.so.tilo.shortcuts.Step`. */
export interface Step extends tilo.Resource {
  h3opbkx5: Step | null;  // parent
  wb3ui23u: Branch;  // branch
  so6ey5xl: string;  // step_id
  vncpsqdq: Head;  // head
  dcuko4cu: string;  // app_id
  gc5jsux7: string;  // action_kind
  srbrc53z: string;  // action_name
  z7jjhgj3: string;  // action_label
  cvrfcqnx: string;  // resource_label
  m4zdkmap: Selector;  // selector
  "7sf54hyp": string;  // target
  "7jcsp3l3": BindValue;  // resource_binding
  dpalqxeo: Array<tilo.Entry<Param>>;  // params
  shmtjvi6: Array<tilo.Entry<Arg>>;  // args
  lldhjl3c: Array<tilo.Entry<Slot>>;  // slots
  rpum3xhw: Condition;  // condition
  e6xcw5u3: string;  // items
  zz64m2bs: string;  // variable
  su76kmlu: string;  // name
  vsalsfek: string;  // value
  szzje4pf: string;  // ask_name
  lryaixlb: InputType;  // ask_type
  nnrjiqq3: string;  // ask_description
  wkqhz6gh: string;  // prompt
  dlqum4fl: BindValue;  // ai_input
  "7kdxq5eg": string;  // ai_output
  xnnuhmxj: string;  // instruction
  rq6c5mlz: string;  // allow
  ptov4iys: string;  // task_output
  yyj53wzo: string;  // read_app
  qd53kkgv: string;  // read_record
  oihaj3wf: boolean;  // read_joined
  "2f76vazs": Policy;  // policy
  ydlvlxqi: number;  // attempts
  w3tgejid: number;  // backoff_ms
  b7i6c2gd: BindValue;  // default_value
  load_arg(args: {
    name: string;
    value: unknown;
  }): Promise<void>;
  /** Runs `load_arg` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_argMany(items: LoadArgArgs[]): Promise<tilo.ActionAnswer[]>;
  /** What an offered action says about a step that calls it: its name, its resource, and what every parameter takes. */
  apply_offer(args: {
    offered: unknown;
  }): Promise<void>;
  /** Runs `apply_offer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  apply_offerMany(items: ApplyOfferArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Step` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STEP_ACTIONS = ["load_arg", "load_argMany", "apply_offer", "apply_offerMany"] as const;

/** The `tl.so.tilo.shortcuts.Step` kind at `path`. */
export function step(ws: tilo.Workspace, path: string): Step {
  return resourceProxy(ws, path, SURFACE, STEP_ACTIONS) as unknown as Step;
}

/** Input — `tl.so.tilo.shortcuts.Input`. */
export interface Input extends tilo.Resource {
  snpnqwju: string;  // name
  tg4tli3b: InputType;  // ty
  m7r4ya7c: boolean;  // required
  jgm4hgap: string;  // description
  zwcz4mo7: string;  // value
}

/** The actions `Input` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const INPUT_ACTIONS = [] as const;

/** The `tl.so.tilo.shortcuts.Input` kind at `path`. */
export function input(ws: tilo.Workspace, path: string): Input {
  return resourceProxy(ws, path, SURFACE, INPUT_ACTIONS) as unknown as Input;
}

/** Trigger — `tl.so.tilo.shortcuts.Trigger`. */
export interface Trigger extends tilo.Resource {
  "5bbzhstm": TriggerKind;  // kind
  nl7rjhv5: string;  // chord
  h6z7jmu7: number;  // hour
  pwego36a: number;  // minute
  izj53iqb: string[];  // weekdays
  w5cphrf7: boolean;  // catch_up
  jwrxcn3c: number;  // period_secs
  mkcu4vtl: number;  // once_ms
  e3uy6zru: string;  // once_text
  "4inykrds": string;  // app_id
  q7czkxzj: string;  // workspace
  ehgpmzgw: string;  // folder
  gcicb4ub: FocusMode;  // focus_mode
  xumiu4qy: FocusTransition;  // focus_transition
  wctuo7yb: string;  // network
  oim6j5ww: Connected;  // connected
  "2yej4ltk": string;  // device_class
  bnuqxwzt: string;  // record_kind
  pxnxp2in: string[];  // actions
  uixffjmo: string[];  // only_fields
  evffas55: string;  // actions_text
  q5adiprn: string;  // fields_text
  "6zzuwtig": By;  // by
  muql6lws: string;  // event_kind
  gavij27k: string;  // from
  x7wd6xhm: string;  // device
  "7xi6qhpz": string;  // time_field
  dcoqx7cm: number;  // offset_secs
  kr5hkco2: string;  // filter
  solyra7s: string;  // source_at
  "5pqw4rrp": number;  // every_secs
  /** Copies every field of one trigger onto another, or the type's own defaults onto it when none is named: a trigger built here is a value no action takes, so the action builds what it copies. */
  apply_trigger(args?: {
    from?: Trigger;
  }): Promise<void>;
  /** Runs `apply_trigger` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  apply_triggerMany(items: ApplyTriggerArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Trigger` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TRIGGER_ACTIONS = ["apply_trigger", "apply_triggerMany"] as const;

/** The `tl.so.tilo.shortcuts.Trigger` kind at `path`. */
export function trigger(ws: tilo.Workspace, path: string): Trigger {
  return resourceProxy(ws, path, SURFACE, TRIGGER_ACTIONS) as unknown as Trigger;
}

/** Offer — `tl.so.tilo.shortcuts.Offer`. */
export interface Offer extends tilo.Resource {
  xitbwh3f: string;  // app_id
  "2oava6yw": string;  // app_label
  zuozugvm: string;  // label
  jufgzesb: string;  // resource
  bzlonbec: string;  // resource_kind
  jqocub4q: string;  // resource_id
  "56tpoja7": string;  // address
  gnvisywl: boolean;  // reads
  fzldwoda: Array<tilo.Entry<Param>>;  // params
}

/** The actions `Offer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OFFER_ACTIONS = [] as const;

/** The `tl.so.tilo.shortcuts.Offer` kind at `path`. */
export function offer(ws: tilo.Workspace, path: string): Offer {
  return resourceProxy(ws, path, SURFACE, OFFER_ACTIONS) as unknown as Offer;
}

/** Program — `tl.so.tilo.shortcuts.Program`. */
export interface Program extends tilo.Resource {
  "6ddumpv3": Array<tilo.Entry<Input>>;  // inputs
  zjl7ntoz: Array<tilo.Entry<Step>>;  // steps
  tndlzxkx: Array<tilo.Entry<Trigger>>;  // triggers
  nvg74t2f: BindValue;  // output
  hzxfqcak: number;  // step_timeout_secs
  ywxim34z: string;  // grant
  /** A value a step's block reads, as a slot: a literal typed, or the binding that holds it. */
  load_slot(args: {
    s: Step;
    name: string;
    value: unknown;
  }): Promise<void>;
  /** Runs `load_slot` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_slotMany(items: LoadSlotArgs[]): Promise<tilo.ActionAnswer[]>;
  load_program(args: {
    item: unknown;
  }): Promise<void>;
  /** Runs `load_program` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_programMany(items: LoadProgramArgs[]): Promise<tilo.ActionAnswer[]>;
  load_block(args: {
    branch: Branch;
    steps?: unknown;
    parent?: Step;
  }): Promise<void>;
  /** Runs `load_block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_blockMany(items: LoadBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  load_step(args: {
    item: unknown;
    branch: Branch;
    parent?: Step;
  }): Promise<void>;
  /** Runs `load_step` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_stepMany(items: LoadStepArgs[]): Promise<tilo.ActionAnswer[]>;
  load_triggers(args: {
    triggers: unknown[];
  }): Promise<void>;
  /** Runs `load_triggers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_triggersMany(items: LoadTriggersArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Program` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROGRAM_ACTIONS = ["load_slot", "load_slotMany", "load_program", "load_programMany", "load_block", "load_blockMany", "load_step", "load_stepMany", "load_triggers", "load_triggersMany"] as const;

/** The `tl.so.tilo.shortcuts.Program` kind at `path`. */
export function program(ws: tilo.Workspace, path: string): Program {
  return resourceProxy(ws, path, SURFACE, PROGRAM_ACTIONS) as unknown as Program;
}

/** Suggested — `tl.so.tilo.shortcuts.Suggested`. */
export interface Suggested extends tilo.Resource {
  m53kboqe: string;  // name
  duorb75u: Program;  // program
  pshhv3vt: string[];  // problems
  "6e4ysabb": string[];  // changes
}

/** The actions `Suggested` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SUGGESTED_ACTIONS = [] as const;

/** The `tl.so.tilo.shortcuts.Suggested` kind at `path`. */
export function suggested(ws: tilo.Workspace, path: string): Suggested {
  return resourceProxy(ws, path, SURFACE, SUGGESTED_ACTIONS) as unknown as Suggested;
}

/** Saved — `tl.so.tilo.shortcuts.Saved`. */
export interface Saved extends tilo.Resource {
  l4rxg53i: string;  // id
  "2hqp5wnu": string;  // name
  tpetr7if: Program;  // program
  fydnutns: boolean;  // paused
  cc5kjsxw: string;  // last_call
  "44lyca4z": number;  // last_started
  /** Reads the program a shortcut is made of into `p`. Reads one saved shortcut's entry in the library into `saved`. */
  load_saved(args: {
    value: unknown;
  }): Promise<void>;
  /** Runs `load_saved` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_savedMany(items: LoadSavedArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Saved` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVED_ACTIONS = ["load_saved", "load_savedMany"] as const;

/** The `tl.so.tilo.shortcuts.Saved` kind at `path`. */
export function saved(ws: tilo.Workspace, path: string): Saved {
  return resourceProxy(ws, path, SURFACE, SAVED_ACTIONS) as unknown as Saved;
}

/** Draft — `tl.so.tilo.shortcuts.Draft`. */
export interface Draft extends tilo.Resource {
  qodc2wld: string;  // name
  imvb73zl: string;  // shortcut_id
  jdjr5ygm: boolean;  // replaces_saved
  qhds5d5u: Program;  // program
  kcuhy6xu: Array<tilo.Entry<Problem>>;  // problems
  uzhz35e3: Array<tilo.Entry<Grant>>;  // grants
  unbpnmsw: string;  // folded
  "2j6kqzzx": boolean;  // as_text
  /** A shortcut written as JSON, loaded into `d` as its steps, inputs and triggers. */
  load_shortcut(args: {
    program: unknown;
  }): Promise<void>;
  /** Runs `load_shortcut` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  load_shortcutMany(items: LoadShortcutArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies the steps of one block, and every block inside them, into another draft. */
  copy_block(args: {
    to: Draft;
    branch: Branch;
    parent?: Step;
  }): Promise<void>;
  /** Runs `copy_block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  copy_blockMany(items: CopyBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A shortcut loaded as a draft arms nothing: a person saves it first, after the review. */
  disarm(): Promise<void>;
  /** Runs `disarm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  disarmMany(items: DisarmArgs[]): Promise<tilo.ActionAnswer[]>;
  remove_below(args: {
    s: Step;
  }): Promise<void>;
  /** Runs `remove_below` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_belowMany(items: RemoveBelowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Check the draft, and what each of its steps' arguments may be bound to, so the editor shows a person what saving would accept without the view reading anything itself. */
  review(): Promise<void>;
  /** Keeps every action step's parameters and the type of each argument what the catalogue says they are, reading the catalogue once. */
  match_catalogue(): Promise<void>;
  /** Runs `match_catalogue` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  match_catalogueMany(items: MatchCatalogueArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Draft` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DRAFT_ACTIONS = ["load_shortcut", "load_shortcutMany", "copy_block", "copy_blockMany", "disarm", "disarmMany", "remove_below", "remove_belowMany", "review", "match_catalogue", "match_catalogueMany"] as const;

/** The `tl.so.tilo.shortcuts.Draft` kind at `path`. */
export function draft(ws: tilo.Workspace, path: string): Draft {
  return resourceProxy(ws, path, SURFACE, DRAFT_ACTIONS) as unknown as Draft;
}

/** Workbench — `tl.so.tilo.shortcuts.Workbench`. */
export interface Workbench extends tilo.Resource {
  rwdjyqtz: Array<tilo.Entry<Draft>>;  // drafts
  daxqqmym: Array<tilo.Entry<Saved>>;  // saved
  fbadmr47: Array<tilo.Entry<Suggested>>;  // suggested
  dmzlirxv: string;  // notice
  ke54krt6: string;  // failure
  vjatpjs3: Trigger;  // form
  td3nielz: Trigger | null;  // editing
  jpfqt6cc: string;  // sheet
  wb5qv6mt: string;  // description
  t2nlvsmx: string;  // comment
  sgiqblcg: string;  // picker_query
  xmyhixxe: string;  // new_input_name
  jnw4b5oc: InputType;  // new_input_type
  olhnz4lr: string[];  // ignored
  "45ccshtx": Array<tilo.Entry<Offer>>;  // picker
  awchuh2j: Draft | null;  // picker_for
  rxujaqef: string;  // picker_block
  new_draft(args?: {
    name?: string;
  }): Promise<void>;
  /** Runs `new_draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  new_draftMany(items: NewDraftArgs[]): Promise<tilo.ActionAnswer[]>;
  rename_draft(args: {
    d: Draft;
    name: string;
  }): Promise<void>;
  duplicate_draft(args: {
    d: Draft;
  }): Promise<void>;
  discard(args: {
    d: Draft;
  }): Promise<void>;
  /** Runs `discard` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  discardMany(items: DiscardArgs[]): Promise<tilo.ActionAnswer[]>;
  draft_shortcut(args: {
    shortcut: string;
  }): Promise<void>;
  open_shortcut(args: {
    shortcut: string;
  }): Promise<void>;
  duplicate_shortcut(args: {
    shortcut: string;
  }): Promise<void>;
  /** Open a shortcut the app proposed, named after the work it noticed, as an unsaved draft. */
  draft_proposal(args: {
    program: string;
  }): Promise<void>;
  ignore_proposal(args: {
    key: string;
  }): Promise<void>;
  /** Runs `ignore_proposal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  ignore_proposalMany(items: IgnoreProposalArgs[]): Promise<tilo.ActionAnswer[]>;
  allow_proposal(args: {
    key: string;
  }): Promise<void>;
  /** Runs `allow_proposal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  allow_proposalMany(items: AllowProposalArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The actions the person is offered, read now, for the open picker. Nothing is kept once the picker closes: what a view draws it read in the action that opened it. */
  open_picker(args: {
    d: Draft;
    block: string;
  }): Promise<void>;
  /** Runs `open_picker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_pickerMany(items: OpenPickerArgs[]): Promise<tilo.ActionAnswer[]>;
  close_picker(): Promise<void>;
  /** Runs `close_picker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  close_pickerMany(items: ClosePickerArgs[]): Promise<tilo.ActionAnswer[]>;
  add_offer(args: {
    d: Draft;
    o: Offer;
  }): Promise<void>;
  /** Add a step that is not an app's action: `if`, `for_each`, `set_var`, `ask`, `ai` or `task`. */
  add_block(args: {
    d: Draft;
    head: Head;
  }): Promise<void>;
  remove_step(args: {
    d: Draft;
    s: Step;
  }): Promise<void>;
  move_step(args: {
    d: Draft;
    s: Step;
    earlier: boolean;
  }): Promise<void>;
  nest_step(args: {
    d: Draft;
    s: Step;
  }): Promise<void>;
  lift_step(args: {
    d: Draft;
    s: Step;
  }): Promise<void>;
  set_policy(args: {
    d: Draft;
    s: Step;
    policy: Policy;
  }): Promise<void>;
  set_output(args: {
    d: Draft;
    choice: Choice;
  }): Promise<void>;
  clear_output(args: {
    d: Draft;
  }): Promise<void>;
  /** Where one value a step reads comes from. */
  set_slot(args: {
    d: Draft;
    s: Step;
    name: string;
    pick: Pick;
    choice: Choice;
  }): Promise<void>;
  type_slot(args: {
    d: Draft;
    s: Step;
    name: string;
  }): Promise<void>;
  set_argument(args: {
    d: Draft;
    s: Step;
    a: Arg;
    pick: Pick;
    choice: Choice;
  }): Promise<void>;
  clear_argument(args: {
    d: Draft;
    s: Step;
    a: Arg;
  }): Promise<void>;
  clear_slot(args: {
    d: Draft;
    s: Step;
    name: string;
  }): Promise<void>;
  type_argument(args: {
    d: Draft;
    s: Step;
    a: Arg;
  }): Promise<void>;
  set_condition_typed(args: {
    d: Draft;
    s: Step;
    text: string;
  }): Promise<void>;
  add_input(args: {
    d: Draft;
    name: string;
    ty: InputType;
  }): Promise<void>;
  remove_input(args: {
    d: Draft;
    i: Input;
  }): Promise<void>;
  add_trigger(args: {
    d: Draft;
    t: Trigger;
  }): Promise<void>;
  /** Add what the form holds to the open draft, or replace the trigger the form is editing. */
  apply_form(args: {
    d: Draft;
  }): Promise<void>;
  /** Load a trigger the draft carries into the form, to change it. */
  edit_trigger(args: {
    t: Trigger;
  }): Promise<void>;
  /** Runs `edit_trigger` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  edit_triggerMany(items: EditTriggerArgs[]): Promise<tilo.ActionAnswer[]>;
  cancel_edit(): Promise<void>;
  /** Runs `cancel_edit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancel_editMany(items: CancelEditArgs[]): Promise<tilo.ActionAnswer[]>;
  remove_trigger(args: {
    d: Draft;
    t: Trigger;
  }): Promise<void>;
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  /** Save the draft with the least grant saving would store, after the same check the editor shows. */
  save(args: {
    d: Draft;
  }): Promise<void>;
  /** Run a saved shortcut with what its inputs hold. */
  run(args: {
    s: Saved;
  }): Promise<void>;
  delete_saved(args: {
    s: Saved;
  }): Promise<void>;
  pause_saved(args: {
    s: Saved;
  }): Promise<void>;
  resume_saved(args: {
    s: Saved;
  }): Promise<void>;
  stop_saved(args: {
    s: Saved;
  }): Promise<void>;
  stop_all(): Promise<void>;
  /** Ask the agent to draft a shortcut doing what the description says. */
  describe(args: {
    description: string;
  }): Promise<void>;
  /** Ask the agent what the shortcut should do differently. */
  refine(args: {
    s: Saved;
    comment: string;
  }): Promise<void>;
  /** Offer what the agent answered as a draft to open, with what the checker says of it. */
  adopt(args: {
    answer: string;
    changes: string[];
  }): Promise<void>;
  open_suggested(args: {
    s: Suggested;
  }): Promise<void>;
  dismiss_suggested(args: {
    s: Suggested;
  }): Promise<void>;
  /** Runs `dismiss_suggested` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismiss_suggestedMany(items: DismissSuggestedArgs[]): Promise<tilo.ActionAnswer[]>;
  toggle_fold(args: {
    d: Draft;
    path: string;
  }): Promise<void>;
  /** Runs `toggle_fold` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_foldMany(items: ToggleFoldArgs[]): Promise<tilo.ActionAnswer[]>;
  show_sheet(args: {
    sheet: string;
  }): Promise<void>;
  /** Runs `show_sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_sheetMany(items: ShowSheetArgs[]): Promise<tilo.ActionAnswer[]>;
  clear_new_input(): Promise<void>;
  /** Runs `clear_new_input` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_new_inputMany(items: ClearNewInputArgs[]): Promise<tilo.ActionAnswer[]>;
  toggle_day(args: {
    day: string;
  }): Promise<void>;
  /** Runs `toggle_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_dayMany(items: ToggleDayArgs[]): Promise<tilo.ActionAnswer[]>;
  dismiss_notice(): Promise<void>;
  /** Runs `dismiss_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismiss_noticeMany(items: DismissNoticeArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Workbench` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKBENCH_ACTIONS = ["new_draft", "new_draftMany", "rename_draft", "duplicate_draft", "discard", "discardMany", "draft_shortcut", "open_shortcut", "duplicate_shortcut", "draft_proposal", "ignore_proposal", "ignore_proposalMany", "allow_proposal", "allow_proposalMany", "open_picker", "open_pickerMany", "close_picker", "close_pickerMany", "add_offer", "add_block", "remove_step", "move_step", "nest_step", "lift_step", "set_policy", "set_output", "clear_output", "set_slot", "type_slot", "set_argument", "clear_argument", "clear_slot", "type_argument", "set_condition_typed", "add_input", "remove_input", "add_trigger", "apply_form", "edit_trigger", "edit_triggerMany", "cancel_edit", "cancel_editMany", "remove_trigger", "save", "run", "delete_saved", "pause_saved", "resume_saved", "stop_saved", "stop_all", "describe", "refine", "adopt", "open_suggested", "dismiss_suggested", "dismiss_suggestedMany", "toggle_fold", "toggle_foldMany", "show_sheet", "show_sheetMany", "clear_new_input", "clear_new_inputMany", "toggle_day", "toggle_dayMany", "dismiss_notice", "dismiss_noticeMany"] as const;

/** The `tl.so.tilo.shortcuts.Workbench` kind at `path`. */
export function workbench(ws: tilo.Workspace, path: string): Workbench {
  return resourceProxy(ws, path, SURFACE, WORKBENCH_ACTIONS) as unknown as Workbench;
}

/** Place — `tl.so.tilo.shortcuts.Place`. */
export interface Place extends tilo.Resource {
  uccg2srx: Step | null;  // parent
  d5s4qkn6: Branch;  // branch
  lmkmjowh: number;  // index
}

/** The actions `Place` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLACE_ACTIONS = [] as const;

/** The `tl.so.tilo.shortcuts.Place` kind at `path`. */
export function place(ws: tilo.Workspace, path: string): Place {
  return resourceProxy(ws, path, SURFACE, PLACE_ACTIONS) as unknown as Place;
}

/** Heard — `tl.so.tilo.shortcuts.Heard`. */
export interface Heard extends tilo.Resource {
  gjnz55fw: string;  // app_id
  ld52mtpg: string;  // action_name
  mflabbsc: string;  // kind
  obs5it3e: string;  // resource
  v3scnpbm: string;  // label
}

/** The actions `Heard` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HEARD_ACTIONS = [] as const;

/** The `tl.so.tilo.shortcuts.Heard` kind at `path`. */
export function heard(ws: tilo.Workspace, path: string): Heard {
  return resourceProxy(ws, path, SURFACE, HEARD_ACTIONS) as unknown as Heard;
}

/** Repeated — `tl.so.tilo.shortcuts.Repeated`. */
export interface Repeated extends tilo.Resource {
  jhdyurec: Heard[];  // steps
  qyjikemy: number;  // times
}

/** The actions `Repeated` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPEATED_ACTIONS = [] as const;

/** The `tl.so.tilo.shortcuts.Repeated` kind at `path`. */
export function repeated(ws: tilo.Workspace, path: string): Repeated {
  return resourceProxy(ws, path, SURFACE, REPEATED_ACTIONS) as unknown as Repeated;
}

/** Shortcuts — `tl.so.tilo.shortcuts`. */
export interface Shortcuts extends tilo.Resource {
  /** The agent's action that drafts, when this workspace is offered it; empty when it is not. */
  agent_address(): Promise<string>;
  /** The actions a draft may use, one line each, as the agent is told about them. */
  catalogue_summary(): Promise<string>;
}

/** The actions `Shortcuts` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHORTCUTS_ACTIONS = ["agent_address", "catalogue_summary"] as const;

/** The `tl.so.tilo.shortcuts` kind at `path`. */
export function shortcuts(ws: tilo.Workspace, path: string): Shortcuts {
  return resourceProxy(ws, path, SURFACE, SHORTCUTS_ACTIONS) as unknown as Shortcuts;
}

