/**
 * Types for Tasks (`so.tilo.lang.tasks`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Task` is a `Resource` of the app's `tl.so.tilo.lang.tasks.Task`, made by its factory with the
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
export const SURFACE = "so.tilo.lang.tasks@dc2b3dd12a37e169";

/** How much a task matters, most urgent last. One of "none", "low", "medium", "high", "urgent". */
export type Priority = "none" | "low" | "medium" | "high" | "urgent";

/** The arguments of `check`. */
export interface CheckArgs {
  done?: boolean;
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
}

/** The arguments of `indent`. */
export interface IndentArgs {
  above?: Task;
}

/** The arguments of `outdent`. */
export interface OutdentArgs {
}

/** The arguments of `place`. */
export interface PlaceArgs {
  at: number;
  s?: Section;
}

/** The arguments of `rename`. */
export interface RenameArgs {
  name: string;
}

/** The arguments of `rename_section`. */
export interface RenameSectionArgs {
  name: string;
}

/** The arguments of `remove_section`. */
export interface RemoveSectionArgs {
}

/** The arguments of `move_to`. */
export interface MoveToArgs {
  moved: Task;
  parent?: Task;
  before?: Task;
}

/** The arguments of `remove_project`. */
export interface RemoveProjectArgs {
  p: Project;
}

/** Task — `tl.so.tilo.lang.tasks.Task`. */
export interface Task extends tilo.Resource {
  oidkt4g5: string;  // title
  fgvmlm5n: boolean;  // done
  g6kykjp2: string | null;  // due
  qadph346: string | null;  // at
  aeilucpg: string | null;  // remind
  ftjnco6i: string;  // notes
  "2eyau2d6": string | null;  // assignee
  fbioszhs: Project | null;  // project
  v56iknh2: Section | null;  // section
  "4sriymst": Priority;  // priority
  "3pf74ifm": Task | null;  // parent
  /** Checks a task off with every task under it, or opens it again with the tasks it is under. */
  check(args?: {
    done?: boolean;
  }): Promise<void>;
  /** Runs `check` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  checkMany(items: CheckArgs[]): Promise<tilo.ActionAnswer[]>;
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
  /** Sets the time of day a task is due at: the day it is on stays, and a task with no day is due on the day the moment names. A reminder that was set for the old moment moves with it, as a day moved by `schedule` keeps its time of day: a task due Wednesday at six, reminded at six, made seven is due at seven and reminded then. */
  set_time(args?: {
    at?: string;
  }): Promise<void>;
  /** Runs `set_time` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_timeMany(items: SetTimeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Clears the day a task is due by: it waits in the inbox, undated. */
  due_clear(): Promise<void>;
  /** Runs `due_clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  due_clearMany(items: DueClearArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the moment whoever is doing a task is reminded of it. A reminder on a day after the task is due moves the task to that day, its moment with it: being told of a task once its day has gone is no reminder, so 'move the reminder to Friday' moves both. */
  remind_at(args: {
    when: string;
  }): Promise<void>;
  /** Runs `remind_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remind_atMany(items: RemindAtArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Clears a task's reminder: it is not told of again. */
  remind_clear(): Promise<void>;
  /** Runs `remind_clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remind_clearMany(items: RemindClearArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Tells whoever is doing a task, or the one who made it, what it is and when it is due: once, and not of a task already done. Choosing the notice opens the task. */
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
  /** Gives a task to someone, or to nobody. */
  assign(args?: {
    who?: string;
  }): Promise<void>;
  /** Runs `assign` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  assignMany(items: AssignArgs[]): Promise<tilo.ActionAnswer[]>;
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
  /** Puts a task into the list `s` of its project's board (none for the first) at the place `at` among the tasks already there: what a card's drag names. */
  place(args: {
    at: number;
    s?: Section;
  }): Promise<void>;
  /** Runs `place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  placeMany(items: PlaceArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Task` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TASK_ACTIONS = ["check", "checkMany", "due_today", "due_todayMany", "due_tomorrow", "due_tomorrowMany", "due_next_week", "due_next_weekMany", "schedule", "scheduleMany", "set_time", "set_timeMany", "due_clear", "due_clearMany", "remind_at", "remind_atMany", "remind_clear", "remind_clearMany", "remind", "remindMany", "set_priority_none", "set_priority_noneMany", "set_priority_low", "set_priority_lowMany", "set_priority_medium", "set_priority_mediumMany", "set_priority_high", "set_priority_highMany", "set_priority_urgent", "set_priority_urgentMany", "file", "fileMany", "unfile", "unfileMany", "assign", "assignMany", "indent", "indentMany", "outdent", "outdentMany", "place", "placeMany"] as const;

/** The `tl.so.tilo.lang.tasks.Task` kind at `path`. */
export function task(ws: tilo.Workspace, path: string): Task {
  return resourceProxy(ws, path, SURFACE, TASK_ACTIONS) as unknown as Task;
}

/** Project — `tl.so.tilo.lang.tasks.Project`. */
export interface Project extends tilo.Resource {
  sgnau7vd: string;  // name
  c5pgzhqt: string;  // notes
  /** Adds a section at the end of a project's board. */
  new_section(): Promise<Section>;
  /** Adds a task to a project, at the end of the list `s` of its board (none for the first). It is the project's primary action, so its key brings the person to the entry that adds there. */
  add_to(args: {
    title: string;
    s?: Section;
  }): Promise<Task>;
  /** Renames a project. */
  rename(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameMany(items: RenameArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Project` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROJECT_ACTIONS = ["new_section", "add_to", "rename", "renameMany"] as const;

/** The `tl.so.tilo.lang.tasks.Project` kind at `path`. */
export function project(ws: tilo.Workspace, path: string): Project {
  return resourceProxy(ws, path, SURFACE, PROJECT_ACTIONS) as unknown as Project;
}

/** Section — `tl.so.tilo.lang.tasks.Section`. */
export interface Section extends tilo.Resource {
  yfd266wj: string;  // name
  ohbjjsu7: Project;  // project
  /** Renames a section. */
  rename_section(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename_section` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_sectionMany(items: RenameSectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a section; its tasks go to the project's first list. */
  remove_section(): Promise<void>;
  /** Runs `remove_section` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_sectionMany(items: RemoveSectionArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Section` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SECTION_ACTIONS = ["rename_section", "rename_sectionMany", "remove_section", "remove_sectionMany"] as const;

/** The `tl.so.tilo.lang.tasks.Section` kind at `path`. */
export function section(ws: tilo.Workspace, path: string): Section {
  return resourceProxy(ws, path, SURFACE, SECTION_ACTIONS) as unknown as Section;
}

/** Tasks — `tl.so.tilo.lang.tasks.Tasks`. */
export interface Tasks extends tilo.Resource {
  lg5gedbh: Array<tilo.Entry<Task>>;  // items
  mdhllpbo: Array<tilo.Entry<Project>>;  // projects
  nk5542m7: Array<tilo.Entry<Section>>;  // sections
  /** Adds a task to the inbox or to `p`, in the list `s` of its board: the entry at a list's end fills `title` with the words nothing else there takes, and the day, moment and person they name, so 'Call the landlord fri 10am @Bo' is a task due Friday at ten, for Bo. */
  new_task(args?: {
    p?: Project;
    s?: Section;
    due?: string;
    at?: string;
    assignee?: string;
    title?: string;
    remind?: string;
  }): Promise<Task>;
  /** Starts a project. */
  new_project(): Promise<Project>;
  /** Adds a subtask under a task, in its project and section, and opens a done one: there is work under it now. */
  add_subtask(args: {
    t: Task;
    title: string;
  }): Promise<Task>;
  /** Moves a task to a place in the outline: at the end of `parent`'s subtasks (the top level for none), or where `before` stands. What is put under a done task opens it. */
  move_to(args: {
    moved: Task;
    parent?: Task;
    before?: Task;
  }): Promise<void>;
  /** Runs `move_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_toMany(items: MoveToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies a task, with every task under it, just after the one it copies. */
  duplicate(args: {
    t: Task;
  }): Promise<Task>;
  // the app's `remove` action is the SDK's own: run it with `action("remove")`.
  /** Deletes every finished task and answers how many went: a finished branch goes whole, and a done task with a step still open under it stays. What was done is the list a person reads back, so this is no command of the bar. */
  clear_done(): Promise<number>;
  /** Deletes a project and its sections; its tasks go back to the inbox. */
  remove_project(args: {
    p: Project;
  }): Promise<void>;
  /** Runs `remove_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_projectMany(items: RemoveProjectArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Tasks` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TASKS_ACTIONS = ["new_task", "new_project", "add_subtask", "move_to", "move_toMany", "duplicate", "clear_done", "remove_project", "remove_projectMany"] as const;

/** The `tl.so.tilo.lang.tasks.Tasks` kind at `path`. */
export function tasks(ws: tilo.Workspace, path: string): Tasks {
  return resourceProxy(ws, path, SURFACE, TASKS_ACTIONS) as unknown as Tasks;
}

/** Tasks — `tl.so.tilo.lang.tasks`. */
export interface LangTasks extends tilo.Resource {
}

/** The actions `LangTasks` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LANGTASKS_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.tasks` kind at `path`. */
export function langTasks(ws: tilo.Workspace, path: string): LangTasks {
  return resourceProxy(ws, path, SURFACE, LANGTASKS_ACTIONS) as unknown as LangTasks;
}

