/**
 * Types for Tasks (`so.tilo.tasks`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Person` is a `Resource` of the app's `tl.so.tilo.tasks.Person`, made by its factory with the
 * resource's path (`tasks/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.tasks@0944081fbec1a28c";

/** Which kind of project this is. One of "project", "area". */
export type Kind = "project" | "area";

/** Where a task stands in its work: a board's columns, fixed so that every task of the workspace reads in the same place in the same column. One of "backlog", "todo", "in_progress", "in_review", "done", "cancelled". */
export type Status = "backlog" | "todo" | "in_progress" | "in_review" | "done" | "cancelled";

/** What a wait waits for: the kinds a session of an agent tells apart. One of "time", "reply", "change", "answer", "people", "pieces". */
export type WaitKind = "time" | "reply" | "change" | "answer" | "people" | "pieces";

/** Why a task's work stopped. One of "failed", "budget", "time", "lost_worker", "person". */
export type StoppedWhy = "failed" | "budget" | "time" | "lost_worker" | "person";

/** Whose budget a task's work spends. One of "person", "agent". */
export type BudgetBy = "person" | "agent";

/** How much a task matters, most urgent last. One of "none", "low", "medium", "high", "urgent". */
export type Priority = "none" | "low" | "medium" | "high" | "urgent";

/** The arguments of `edit-block`. */
export interface EditBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `insert-block`. */
export interface InsertBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `delete-block`. */
export interface DeleteBlockArgs {
  block: string;
}

/** The arguments of `set_kind`. */
export interface SetKindArgs {
  kind: Kind;
}

/** The arguments of `rename`. */
export interface RenameArgs {
  name: string;
}

/** The arguments of `involve`. */
export interface InvolveArgs {
  name: string;
  role?: string;
}

/** The arguments of `remove_person`. */
export interface RemovePersonArgs {
  person: Person;
}

/** The arguments of `remove_project`. */
export interface RemoveProjectArgs {
}

/** The arguments of `edit-block`. */
export interface TaskEditBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `insert-block`. */
export interface TaskInsertBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `delete-block`. */
export interface TaskDeleteBlockArgs {
  block: string;
}

/** The arguments of `retitle`. */
export interface RetitleArgs {
  title: string;
}

/** The arguments of `move_to`. */
export interface MoveToArgs {
  parent?: Task;
  before?: Task;
}

/** The arguments of `indent`. */
export interface IndentArgs {
  above?: Task;
}

/** The arguments of `outdent`. */
export interface OutdentArgs {
}

/** The arguments of `set_status`. */
export interface SetStatusArgs {
  status: Status;
  at?: number;
}

/** The arguments of `check`. */
export interface CheckArgs {
  done?: boolean;
}

/** The arguments of `remove_empty_step`. */
export interface RemoveEmptyStepArgs {
}

/** The arguments of `due_today`. */
export interface DueTodayArgs {
}

/** The arguments of `due_tomorrow`. */
export interface DueTomorrowArgs {
}

/** The arguments of `due_next_week`. */
export interface DueNextWeekArgs {
}

/** The arguments of `schedule`. */
export interface ScheduleArgs {
  due?: string;
}

/** The arguments of `set_time`. */
export interface SetTimeArgs {
  at?: string;
}

/** The arguments of `due_clear`. */
export interface DueClearArgs {
}

/** The arguments of `remind_at`. */
export interface RemindAtArgs {
  when: string;
}

/** The arguments of `remind_clear`. */
export interface RemindClearArgs {
}

/** The arguments of `remind`. */
export interface RemindArgs {
}

/** The arguments of `set_priority_none`. */
export interface SetPriorityNoneArgs {
}

/** The arguments of `set_priority_low`. */
export interface SetPriorityLowArgs {
}

/** The arguments of `set_priority_medium`. */
export interface SetPriorityMediumArgs {
}

/** The arguments of `set_priority_high`. */
export interface SetPriorityHighArgs {
}

/** The arguments of `set_priority_urgent`. */
export interface SetPriorityUrgentArgs {
}

/** The arguments of `estimate`. */
export interface EstimateArgs {
  points?: number;
}

/** The arguments of `file`. */
export interface FileArgs {
  p: Project;
}

/** The arguments of `unfile`. */
export interface UnfileArgs {
}

/** The arguments of `assign`. */
export interface AssignArgs {
  who?: string;
  contact?: unknown;
}

/** The arguments of `relate`. */
export interface RelateArgs {
  other: Task;
  how: string;
}

/** The arguments of `unrelate`. */
export interface UnrelateArgs {
  relation: Related;
}

/** The arguments of `label`. */
export interface LabelArgs {
  name: string;
}

/** The arguments of `unlabel`. */
export interface UnlabelArgs {
  name: string;
}

/** The arguments of `attach`. */
export interface AttachArgs {
  file: unknown;
}

/** The arguments of `detach`. */
export interface DetachArgs {
  attachment: unknown;
}

/** The arguments of `wait`. */
export interface WaitArgs {
  what: string;
  until?: string;
  kind?: WaitKind;
  on?: string;
}

/** The arguments of `resume`. */
export interface ResumeArgs {
  cause?: string;
}

/** The arguments of `set_place`. */
export interface SetPlaceArgs {
  folder?: string;
  note?: string;
}

/** The arguments of `set_budget`. */
export interface SetBudgetArgs {
  micros?: number;
  millis?: number;
  by?: BudgetBy;
}

/** The arguments of `set_pieces_at_once`. */
export interface SetPiecesAtOnceArgs {
  pieces?: number;
}

/** The arguments of `session_began`. */
export interface SessionBeganArgs {
}

/** The arguments of `session_ended`. */
export interface SessionEndedArgs {
  spent_micros?: number;
  worked_ms?: number;
}

/** The arguments of `stop`. */
export interface StopArgs {
  why: StoppedWhy;
  words?: string;
}

/** The arguments of `share_with`. */
export interface ShareWithArgs {
  from: Task;
}

/** The arguments of `set_status_many`. */
export interface SetStatusManyArgs {
  chosen: Task[];
  status: Status;
  at?: number;
}

/** The arguments of `assign_many`. */
export interface AssignManyArgs {
  chosen: Task[];
  who?: string;
}

/** The arguments of `label_many`. */
export interface LabelManyArgs {
  chosen: Task[];
  name: string;
}

/** Person — `tl.so.tilo.tasks.Person`. */
export interface Person extends tilo.Resource {
  ktw2jimm: string;  // name
  hgptkohm: string;  // role
}

/** The actions `Person` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PERSON_ACTIONS = [] as const;

/** The `tl.so.tilo.tasks.Person` kind at `path`. */
export function person(ws: tilo.Workspace, path: string): Person {
  return resourceProxy(ws, path, SURFACE, PERSON_ACTIONS) as unknown as Person;
}

/** Project — `tl.so.tilo.tasks.Project`. */
export interface Project extends tilo.Resource {
  sgnau7vd: string;  // name
  rsoasw3n: Kind;  // kind
  c5pgzhqt: string;  // notes
  mboq6kfe: Array<tilo.Entry<Person>>;  // people
  /** Set one paragraph of a rich-text field to what a Markdown text says. */
  editBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editBlockMany(items: EditBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Put a paragraph before the one the address names. */
  insertBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertBlockMany(items: InsertBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remove the paragraph the address names, and the paragraphs it holds. */
  deleteBlock(args: {
    block: string;
  }): Promise<void>;
  /** Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteBlockMany(items: DeleteBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets whether a project is one with an end, or an ongoing responsibility. */
  set_kind(args: {
    kind: Kind;
  }): Promise<void>;
  /** Runs `set_kind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_kindMany(items: SetKindArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames a project. */
  rename(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameMany(items: RenameArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds someone involved in a project, with the role they have there. */
  involve(args: {
    name: string;
    role?: string;
  }): Promise<void>;
  /** Runs `involve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  involveMany(items: InvolveArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes someone off a project. */
  remove_person(args: {
    person: Person;
  }): Promise<void>;
  /** Runs `remove_person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_personMany(items: RemovePersonArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a project; its tasks go back to the inbox. */
  remove_project(): Promise<void>;
  /** Runs `remove_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_projectMany(items: RemoveProjectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a task to a project at `status`. It is the project's primary action, so its key brings the person to the entry that adds there. */
  add_to(args: {
    title: string;
    status?: Status;
  }): Promise<Task>;
}

/** The actions `Project` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROJECT_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "set_kind", "set_kindMany", "rename", "renameMany", "involve", "involveMany", "remove_person", "remove_personMany", "remove_project", "remove_projectMany", "add_to"] as const;

/** The `tl.so.tilo.tasks.Project` kind at `path`. */
export function project(ws: tilo.Workspace, path: string): Project {
  return resourceProxy(ws, path, SURFACE, PROJECT_ACTIONS) as unknown as Project;
}

/** Wait — `tl.so.tilo.tasks.Wait`. */
export interface Wait extends tilo.Resource {
  "4fz6j3rg": WaitKind;  // kind
  "4ijm32x7": string;  // what
  dy6nui5s: string | null;  // until
  osxm3ega: string;  // event
}

/** The actions `Wait` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WAIT_ACTIONS = [] as const;

/** The `tl.so.tilo.tasks.Wait` kind at `path`. */
export function wait(ws: tilo.Workspace, path: string): Wait {
  return resourceProxy(ws, path, SURFACE, WAIT_ACTIONS) as unknown as Wait;
}

/** Stopped — `tl.so.tilo.tasks.Stopped`. */
export interface Stopped extends tilo.Resource {
  ps5lbw2w: StoppedWhy;  // why
  "5yqkyqzr": string;  // words
  kdugvhf2: string;  // at
}

/** The actions `Stopped` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STOPPED_ACTIONS = [] as const;

/** The `tl.so.tilo.tasks.Stopped` kind at `path`. */
export function stopped(ws: tilo.Workspace, path: string): Stopped {
  return resourceProxy(ws, path, SURFACE, STOPPED_ACTIONS) as unknown as Stopped;
}

/** Resumed — `tl.so.tilo.tasks.Resumed`. */
export interface Resumed extends tilo.Resource {
  "66a5tqcs": string;  // cause
  ys7neba6: string;  // at
}

/** The actions `Resumed` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RESUMED_ACTIONS = [] as const;

/** The `tl.so.tilo.tasks.Resumed` kind at `path`. */
export function resumed(ws: tilo.Workspace, path: string): Resumed {
  return resourceProxy(ws, path, SURFACE, RESUMED_ACTIONS) as unknown as Resumed;
}

/** Work — `tl.so.tilo.tasks.Work`. */
export interface Work extends tilo.Resource {
  finhtl5k: string;  // folder
  ckijp22f: string;  // note
  "3hituttb": Wait[];  // waits
  fxyk36kk: string | null;  // until
  pcmbdaod: string | null;  // since
  lkkui5te: number | null;  // budget_micros
  o5tuxt2b: number | null;  // budget_ms
  byxiukp2: BudgetBy;  // budget_by
  "4c4aw4xd": number;  // spent_micros
  virxpj23: number;  // worked_ms
  hztvc5dj: number;  // sessions
  nuikd6fz: string | null;  // session_since
  qublsm3d: number;  // lost
  nax7z7kw: Stopped | null;  // stopped
  "55waklcc": Resumed | null;  // resumed
  huwemkpv: number | null;  // pieces_at_once
}

/** The actions `Work` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORK_ACTIONS = [] as const;

/** The `tl.so.tilo.tasks.Work` kind at `path`. */
export function work(ws: tilo.Workspace, path: string): Work {
  return resourceProxy(ws, path, SURFACE, WORK_ACTIONS) as unknown as Work;
}

/** Origin — `tl.so.tilo.tasks.Origin`. */
export interface Origin extends tilo.Resource {
  "3saq3nij": string;  // author
  duzjakqx: string | null;  // handle
  g6jrgwt6: string;  // source
  gncjc3dz: string | null;  // url
  qg5qvqsi: string | null;  // reference
  xuslp5pg: string | null;  // created
  no47vg4q: string | null;  // updated
  import_issue(args: {
    title: string;
    key: string;
    status?: Status;
  }): Promise<Task>;
}

/** The actions `Origin` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ORIGIN_ACTIONS = ["import_issue"] as const;

/** The `tl.so.tilo.tasks.Origin` kind at `path`. */
export function tasksOrigin(ws: tilo.Workspace, path: string): Origin {
  return resourceProxy(ws, path, SURFACE, ORIGIN_ACTIONS) as unknown as Origin;
}

/** Task — `tl.so.tilo.tasks.Task`. */
export interface Task extends tilo.Resource {
  "2y3yxgz4": number;  // number
  oidkt4g5: string;  // title
  ftjnco6i: string;  // notes
  "2xssorsk": Status;  // status
  fgvmlm5n: boolean;  // done
  g6kykjp2: string | null;  // due
  qadph346: string | null;  // at
  aeilucpg: string | null;  // remind
  "2eyau2d6": string | null;  // assignee
  j3fym73f: unknown | null;  // contact
  ptyivutv: string | null;  // assigned_by
  r5bd6ss3: string | null;  // assigned_at
  tj32pwsz: string[];  // labels
  fbioszhs: Project | null;  // project
  "4sriymst": Priority;  // priority
  nc5peol7: number | null;  // estimate
  "3pf74ifm": Task | null;  // parent
  zjuuqsza: unknown[];  // attachments
  sqy7cbtd: Origin | null;  // origin
  o6ijslz7: string | null;  // url
  "3uvt7tnr": Work | null;  // work
  uxlestrl: string;  // waiting_for
  m6raucmj: string | null;  // waiting_until
  vtgi2fsa: string;  // waiting_on
  "5ltuj6t2": Array<tilo.Entry<Related>>;  // related
  /** Set one paragraph of a rich-text field to what a Markdown text says. */
  editBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editBlockMany(items: TaskEditBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Put a paragraph before the one the address names. */
  insertBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertBlockMany(items: TaskInsertBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remove the paragraph the address names, and the paragraphs it holds. */
  deleteBlock(args: {
    block: string;
  }): Promise<void>;
  /** Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteBlockMany(items: TaskDeleteBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets what a task is called. */
  retitle(args: {
    title: string;
  }): Promise<void>;
  /** Runs `retitle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  retitleMany(items: RetitleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a task to a place in the outline: at the end of `parent`'s subtasks (the top level for none), or where `before` stands. What is put under a closed task opens it. */
  move_to(args?: {
    parent?: Task;
    before?: Task;
  }): Promise<void>;
  /** Runs `move_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_toMany(items: MoveToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a task under the task the view shows just over it in its group, as `Ctrl+]` does; with none named, under the task above it in the list. */
  indent(args?: {
    above?: Task;
  }): Promise<void>;
  /** Runs `indent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  indentMany(items: IndentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a task out from under its parent, just after it, as `Ctrl+[` does in an outline. */
  outdent(): Promise<void>;
  /** Runs `outdent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  outdentMany(items: OutdentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a task in a column of the board, at the place `at` among the tasks already there: what a card's drag names. */
  set_status(args: {
    status: Status;
    at?: number;
  }): Promise<void>;
  /** Runs `set_status` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_statusMany(items: SetStatusArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Checks a task off with every task under it, or opens it again with the tasks it is under. */
  check(args?: {
    done?: boolean;
  }): Promise<void>;
  /** Runs `check` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  checkMany(items: CheckArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a subtask under a task, in its project and column, and opens a closed one: there is work under it now. */
  add_subtask(args: {
    title: string;
  }): Promise<Task>;
  /** Takes away the subtask the entry left empty: a field that began one and was left blank is no step of the work, so nothing empty stands under the task. */
  remove_empty_step(): Promise<void>;
  /** Runs `remove_empty_step` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_empty_stepMany(items: RemoveEmptyStepArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the day a task is due by, from the presets or a day the person picks. */
  due_today(): Promise<void>;
  /** Runs `due_today` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  due_todayMany(items: DueTodayArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a task due tomorrow. */
  due_tomorrow(): Promise<void>;
  /** Runs `due_tomorrow` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  due_tomorrowMany(items: DueTomorrowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a task due at the start of next week. */
  due_next_week(): Promise<void>;
  /** Runs `due_next_week` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  due_next_weekMany(items: DueNextWeekArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a task due on a day the person picks. Its moment and its reminder move with it, at the same time of day: a task due Wednesday at six, reminded at six, moved to Friday is due Friday at six and reminded then. */
  schedule(args?: {
    due?: string;
  }): Promise<void>;
  /** Runs `schedule` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  scheduleMany(items: ScheduleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the time of day a task is due at: the day it is on stays, and a task with no day is due on the day the moment names. A reminder that was set for the old moment moves with it, as a day moved by `schedule` keeps its time of day. */
  set_time(args?: {
    at?: string;
  }): Promise<void>;
  /** Runs `set_time` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_timeMany(items: SetTimeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Clears the day a task is due by: it waits in the inbox, undated. */
  due_clear(): Promise<void>;
  /** Runs `due_clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  due_clearMany(items: DueClearArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the moment whoever is doing a task is reminded of it. A reminder on a day after the task is due moves the task to that day, its moment with it. */
  remind_at(args: {
    when: string;
  }): Promise<void>;
  /** Runs `remind_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remind_atMany(items: RemindAtArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Clears a task's reminder: it is not told of again. */
  remind_clear(): Promise<void>;
  /** Runs `remind_clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remind_clearMany(items: RemindClearArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Tells whoever is doing a task, or the one who made it, what it is and when it is due: once, and not of a task already closed. Choosing the notice opens the task. */
  remind(): Promise<void>;
  /** Runs `remind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remindMany(items: RemindArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets how much a task matters. */
  set_priority_none(): Promise<void>;
  /** Runs `set_priority_none` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_priority_noneMany(items: SetPriorityNoneArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Marks a task as low priority. */
  set_priority_low(): Promise<void>;
  /** Runs `set_priority_low` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_priority_lowMany(items: SetPriorityLowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Marks a task as medium priority. */
  set_priority_medium(): Promise<void>;
  /** Runs `set_priority_medium` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_priority_mediumMany(items: SetPriorityMediumArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Marks a task as high priority. */
  set_priority_high(): Promise<void>;
  /** Runs `set_priority_high` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_priority_highMany(items: SetPriorityHighArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Marks a task as urgent. */
  set_priority_urgent(): Promise<void>;
  /** Runs `set_priority_urgent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_priority_urgentMany(items: SetPriorityUrgentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the points a task is worth, or none. */
  estimate(args?: {
    points?: number;
  }): Promise<void>;
  /** Runs `estimate` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  estimateMany(items: EstimateArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Files a task under a project the person picks. */
  file(args: {
    p: Project;
  }): Promise<void>;
  /** Runs `file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  fileMany(items: FileArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a task out of its project, back to the inbox. */
  unfile(): Promise<void>;
  /** Runs `unfile` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unfileMany(items: UnfileArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a task to someone, or to nobody: a member of the workspace by their actor, or someone the workspace keeps in touch with by their contact. Who gave it to an agent, and when, stay on the task: the assignment is the change the agent's own turn is triggered by. */
  assign(args?: {
    who?: string;
    contact?: unknown;
  }): Promise<void>;
  /** Runs `assign` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  assignMany(items: AssignArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Links a task to another, with how they relate: the workspace's own link between two tasks, which the page of the task it relates to draws too. */
  relate(args: {
    other: Task;
    how: string;
  }): Promise<void>;
  /** Runs `relate` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  relateMany(items: RelateArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a relation off a task. */
  unrelate(args: {
    relation: Related;
  }): Promise<void>;
  /** Runs `unrelate` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unrelateMany(items: UnrelateArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a task a label. */
  label(args: {
    name: string;
  }): Promise<void>;
  /** Runs `label` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  labelMany(items: LabelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a label off a task. */
  unlabel(args: {
    name: string;
  }): Promise<void>;
  /** Runs `unlabel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unlabelMany(items: UnlabelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Attaches a file to a task. */
  attach(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `attach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  attachMany(items: AttachArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes an attachment off a task. */
  detach(args: {
    attachment: unknown;
  }): Promise<void>;
  /** Runs `detach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  detachMany(items: DetachArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the attachment `attachment` in the app that opens its kind: the file a task holds reaches that app as the same content both read, never as a path neither has. */
  open_attachment(args: {
    attachment: unknown;
  }): Promise<void>;
  /** Writes a task's note in Notes: a note named like the task, holding what it remembers, made from the task so it reads under it. */
  write_note(): Promise<unknown>;
  /** Puts a task on hold until what it waits for comes, or until `until` at the latest: whoever is doing it goes on with it then, a person or an agent alike. `on` names the automation that ends the wait early when it runs. */
  wait(args: {
    what: string;
    until?: string;
    kind?: WaitKind;
    on?: string;
  }): Promise<void>;
  /** Runs `wait` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  waitMany(items: WaitArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Ends every wait a task is on: what it waited for came, which `cause` says in a few words, and whoever is doing it goes on with it. It is also how a person starts its work over after its sessions were lost too often: the losses no longer stand in a row, and a session may begin. */
  resume(args?: {
    cause?: string;
  }): Promise<void>;
  /** Runs `resume` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resumeMany(items: ResumeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets where a task's working material is: the folder of its work, and the note its sessions write. */
  set_place(args?: {
    folder?: string;
    note?: string;
  }): Promise<void>;
  /** Runs `set_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_placeMany(items: SetPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets what a task's work may spend and how long it may work, and whose budget it charges. */
  set_budget(args?: {
    micros?: number;
    millis?: number;
    by?: BudgetBy;
  }): Promise<void>;
  /** Runs `set_budget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_budgetMany(items: SetBudgetArgs[]): Promise<tilo.ActionAnswer[]>;
  /** How many of a task's pieces may be in work at once. */
  set_pieces_at_once(args?: {
    pieces?: number;
  }): Promise<void>;
  /** Runs `set_pieces_at_once` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_pieces_at_onceMany(items: SetPiecesAtOnceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Says a session on a task began: counted, and its start kept for how long it works. */
  session_began(): Promise<void>;
  /** Runs `session_began` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  session_beganMany(items: SessionBeganArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Says a session on a task ended: what it spent and how long it worked are added to the task. */
  session_ended(args?: {
    spent_micros?: number;
    worked_ms?: number;
  }): Promise<void>;
  /** Runs `session_ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  session_endedMany(items: SessionEndedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Says a task's work stopped, and why, in the words its person reads. */
  stop(args: {
    why: StoppedWhy;
    words?: string;
  }): Promise<void>;
  /** Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stopMany(items: StopArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies a task, with every task under it, just after the one it copies. */
  duplicate(): Promise<Task>;
  // the app's `remove` action is the SDK's own: run it with `action("remove")`.
  /** Gives the copy `to` what `from` holds in its set, its list and its collection: what only an action may change. */
  share_with(args: {
    from: Task;
  }): Promise<void>;
  /** Runs `share_with` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  share_withMany(items: ShareWithArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Task` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TASK_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "retitle", "retitleMany", "move_to", "move_toMany", "indent", "indentMany", "outdent", "outdentMany", "set_status", "set_statusMany", "check", "checkMany", "add_subtask", "remove_empty_step", "remove_empty_stepMany", "due_today", "due_todayMany", "due_tomorrow", "due_tomorrowMany", "due_next_week", "due_next_weekMany", "schedule", "scheduleMany", "set_time", "set_timeMany", "due_clear", "due_clearMany", "remind_at", "remind_atMany", "remind_clear", "remind_clearMany", "remind", "remindMany", "set_priority_none", "set_priority_noneMany", "set_priority_low", "set_priority_lowMany", "set_priority_medium", "set_priority_mediumMany", "set_priority_high", "set_priority_highMany", "set_priority_urgent", "set_priority_urgentMany", "estimate", "estimateMany", "file", "fileMany", "unfile", "unfileMany", "assign", "assignMany", "relate", "relateMany", "unrelate", "unrelateMany", "label", "labelMany", "unlabel", "unlabelMany", "attach", "attachMany", "detach", "detachMany", "open_attachment", "write_note", "wait", "waitMany", "resume", "resumeMany", "set_place", "set_placeMany", "set_budget", "set_budgetMany", "set_pieces_at_once", "set_pieces_at_onceMany", "session_began", "session_beganMany", "session_ended", "session_endedMany", "stop", "stopMany", "duplicate", "share_with", "share_withMany"] as const;

/** The `tl.so.tilo.tasks.Task` kind at `path`. */
export function task(ws: tilo.Workspace, path: string): Task {
  return resourceProxy(ws, path, SURFACE, TASK_ACTIONS) as unknown as Task;
}

/** Related — `tl.so.tilo.tasks.Related`. */
export interface Related extends tilo.Resource {
  h62xalud: Task;  // task
  "27gmtaed": string;  // how
}

/** The actions `Related` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RELATED_ACTIONS = [] as const;

/** The `tl.so.tilo.tasks.Related` kind at `path`. */
export function related(ws: tilo.Workspace, path: string): Related {
  return resourceProxy(ws, path, SURFACE, RELATED_ACTIONS) as unknown as Related;
}

/** Tasks — `tl.so.tilo.tasks.Tasks`. */
export interface Tasks extends tilo.Resource {
  lg5gedbh: Array<tilo.Entry<Task>>;  // items
  mdhllpbo: Array<tilo.Entry<Project>>;  // projects
  c7peh6l3: Array<tilo.Entry<Origin>>;  // origins
  auinwnh3: number;  // numbers
  /** Adds a task to the inbox or to `p` at `status`: the entry at a list's end fills `title` with the words nothing else there takes, and the day, moment and person they name, so 'Call the landlord fri 10am @Bo' is a task due Friday at ten, for Bo. */
  new_task(args?: {
    p?: Project;
    status?: Status;
    due?: string;
    at?: string;
    assignee?: string;
    title?: string;
    remind?: string;
    notes?: string;
  }): Promise<Task>;
  /** Starts a project, with what it is for when the caller says. */
  new_project(args?: {
    notes?: string;
  }): Promise<Project>;
  /** Starts a task's work on another tracker's issue: its origin, its page there and its number, and a task of the same origin already here is that task, so importing it again makes none. Names where a task imported here was first written: one origin per place it came from. */
  add_origin(args: {
    author: string;
    source: string;
    handle?: string;
    url?: string;
    reference?: string;
    created?: string;
    updated?: string;
  }): Promise<Origin>;
  /** Deletes every finished task and answers how many went: a finished branch goes whole, and a done task with a step still open under it stays. */
  clear_done(): Promise<number>;
}

/** The actions `Tasks` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TASKS_ACTIONS = ["new_task", "new_project", "add_origin", "clear_done"] as const;

/** The `tl.so.tilo.tasks.Tasks` kind at `path`. */
export function tasks(ws: tilo.Workspace, path: string): Tasks {
  return resourceProxy(ws, path, SURFACE, TASKS_ACTIONS) as unknown as Tasks;
}

/** Tasks — `tl.so.tilo.tasks`. */
export interface TiloTasks extends tilo.Resource {
  /** Adds every task of `titles` at once, as one change: none of them is made when one cannot be (a blank title), so a person's batch is whole or nothing. */
  new_tasks(args: {
    titles: string[];
    p?: Project;
  }): Promise<Task[]>;
  /** Puts every chosen task in one column (or project) at once, as one change. */
  set_status_many(args: {
    chosen: Task[];
    status: Status;
    at?: number;
  }): Promise<void>;
  /** Runs `set_status_many` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_status_manyMany(items: SetStatusManyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives every chosen task to one person (or nobody), as one change. */
  assign_many(args: {
    chosen: Task[];
    who?: string;
  }): Promise<void>;
  /** Runs `assign_many` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  assign_manyMany(items: AssignManyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts every chosen task under one label, as one change. */
  label_many(args: {
    chosen: Task[];
    name: string;
  }): Promise<void>;
  /** Runs `label_many` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  label_manyMany(items: LabelManyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Imports files as tasks: one task for each file, under the project `p` when one is named, the file attached to it. A file of the same name already carried by a task is not imported twice. */
  import_files(args: {
    files: unknown[];
    p?: Project;
  }): Promise<Task[]>;
}

/** The actions `TiloTasks` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOTASKS_ACTIONS = ["new_tasks", "set_status_many", "set_status_manyMany", "assign_many", "assign_manyMany", "label_many", "label_manyMany", "import_files"] as const;

/** The `tl.so.tilo.tasks` kind at `path`. */
export function tiloTasks(ws: tilo.Workspace, path: string): TiloTasks {
  return resourceProxy(ws, path, SURFACE, TILOTASKS_ACTIONS) as unknown as TiloTasks;
}

