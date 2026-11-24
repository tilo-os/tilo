/**
 * Types for Calendar (`so.tilo.calendar`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Attendee` is a `Resource` of the app's `tl.so.tilo.calendar.Attendee`, made by its factory with the
 * resource's path (`calendar/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.calendar@7fd976639ef7547f";

/** The colours a calendar, an event or a project can take, each a place in the theme. One of "indigo", "emerald", "amber", "red", "blue", "violet", "slate". */
export type Color = "indigo" | "emerald" | "amber" | "red" | "blue" | "violet" | "slate";

/** How much a task matters, most urgent last. One of "none", "low", "medium", "high". */
export type Priority = "none" | "low" | "medium" | "high";

/** How an event repeats: every day, week, month or year, or on chosen days of the week. One of "none", "daily", "weekly", "monthly", "yearly", "weekdays". */
export type Repeat = "none" | "daily" | "weekly" | "monthly" | "yearly" | "weekdays";

/** The arguments of `set_project_color`. */
export interface SetProjectColorArgs {
  color: Color;
}

/** The arguments of `rename_project`. */
export interface RenameProjectArgs {
  name: string;
}

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

/** The arguments of `check_task`. */
export interface CheckTaskArgs {
  done?: boolean;
}

/** The arguments of `due_task`. */
export interface DueTaskArgs {
  due?: string;
}

/** The arguments of `due_task_today`. */
export interface DueTaskTodayArgs {
}

/** The arguments of `due_task_tomorrow`. */
export interface DueTaskTomorrowArgs {
}

/** The arguments of `due_task_next_week`. */
export interface DueTaskNextWeekArgs {
}

/** The arguments of `due_task_clear`. */
export interface DueTaskClearArgs {
}

/** The arguments of `set_task_priority`. */
export interface SetTaskPriorityArgs {
  priority: Priority;
}

/** The arguments of `file_task`. */
export interface FileTaskArgs {
  p?: Project;
}

/** The arguments of `edit-block`. */
export interface EventEditBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `insert-block`. */
export interface EventInsertBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `delete-block`. */
export interface EventDeleteBlockArgs {
  block: string;
}

/** The arguments of `dropped`. */
export interface DroppedArgs {
  start: string;
  place: string;
}

/** The arguments of `ended`. */
export interface EndedArgs {
  end_at: string;
}

/** The arguments of `file_event`. */
export interface FileEventArgs {
  p?: Project;
}

/** The arguments of `set_event_color`. */
export interface SetEventColorArgs {
  color: Color;
}

/** The arguments of `clear_event_color`. */
export interface ClearEventColorArgs {
}

/** The arguments of `set_categories`. */
export interface SetCategoriesArgs {
  categories: string[];
}

/** The arguments of `set_zone`. */
export interface SetZoneArgs {
  zone: string;
}

/** The arguments of `add_category`. */
export interface AddCategoryArgs {
  category: string;
}

/** The arguments of `remove_category`. */
export interface RemoveCategoryArgs {
  category: string;
}

/** The arguments of `organize`. */
export interface OrganizeArgs {
  who: string;
}

/** The arguments of `remind_at_start`. */
export interface RemindAtStartArgs {
}

/** The arguments of `remind_before`. */
export interface RemindBeforeArgs {
  minutes: number;
}

/** The arguments of `remind_off`. */
export interface RemindOffArgs {
}

/** The arguments of `repeat_rule`. */
export interface RepeatRuleArgs {
  how: Repeat;
  every?: number;
  weekdays?: string[];
  until?: string;
  count?: number;
}

/** The arguments of `repeat_weekdays`. */
export interface RepeatWeekdaysArgs {
  days: string[];
}

/** The arguments of `repeat_off`. */
export interface RepeatOffArgs {
}

/** The arguments of `skip_date`. */
export interface SkipDateArgs {
  day: string;
}

/** The arguments of `restore_date`. */
export interface RestoreDateArgs {
  day: string;
}

/** The arguments of `move_occurrence`. */
export interface MoveOccurrenceArgs {
  at: string;
  start: string;
  end: string;
}

/** The arguments of `cancel_occurrence`. */
export interface CancelOccurrenceArgs {
  at: string;
}

/** The arguments of `restore_occurrence`. */
export interface RestoreOccurrenceArgs {
  at: string;
}

/** The arguments of `move_following`. */
export interface MoveFollowingArgs {
  at: string;
  start: string;
  end: string;
}

/** The arguments of `move_to_day`. */
export interface MoveToDayArgs {
  day: string;
}

/** The arguments of `change_day`. */
export interface ChangeDayArgs {
  at: string;
}

/** The arguments of `change_start`. */
export interface ChangeStartArgs {
  at: string;
}

/** The arguments of `change_end`. */
export interface ChangeEndArgs {
  at: string;
}

/** The arguments of `move_end_day`. */
export interface MoveEndDayArgs {
  at: string;
}

/** The arguments of `repeat`. */
export interface RepeatArgs {
  how: Repeat;
}

/** The arguments of `repeat_every`. */
export interface RepeatEveryArgs {
  every: number;
}

/** The arguments of `repeat_until`. */
export interface RepeatUntilArgs {
  until?: string;
}

/** The arguments of `repeat_until_at`. */
export interface RepeatUntilAtArgs {
  at: string;
}

/** The arguments of `repeat_count`. */
export interface RepeatCountArgs {
  count?: number;
}

/** The arguments of `file_in`. */
export interface FileInArgs {
  k: Calendar;
}

/** The arguments of `answer`. */
export interface AnswerArgs {
  g: Attendee;
  status: string;
}

/** The arguments of `uninvite`. */
export interface UninviteArgs {
  g: Attendee;
}

/** The arguments of `invite_by_mail`. */
export interface InviteByMailArgs {
  g: Attendee;
}

/** The arguments of `ring_due`. */
export interface RingDueArgs {
}

/** The arguments of `remind_now`. */
export interface RemindNowArgs {
}

/** The arguments of `go_to`. */
export interface GoToArgs {
}

/** The arguments of `repeat_every_text`. */
export interface RepeatEveryTextArgs {
  typed: string;
}

/** The arguments of `repeat_count_text`. */
export interface RepeatCountTextArgs {
  typed: string;
}

/** The arguments of `set_calendar_color`. */
export interface SetCalendarColorArgs {
  color: Color;
}

/** The arguments of `rename_calendar`. */
export interface RenameCalendarArgs {
  name: string;
}

/** The arguments of `remove_task`. */
export interface RemoveTaskArgs {
  t: Task;
}

/** The arguments of `move_project`. */
export interface MoveProjectArgs {
  p: Project;
  up: boolean;
}

/** The arguments of `remove_project`. */
export interface RemoveProjectArgs {
  p: Project;
}

/** The arguments of `go_to_day`. */
export interface GoToDayArgs {
  at: string;
}

/** The arguments of `previous_day`. */
export interface PreviousDayArgs {
}

/** The arguments of `next_day`. */
export interface NextDayArgs {
}

/** The arguments of `this_day`. */
export interface ThisDayArgs {
}

/** The arguments of `previous_week`. */
export interface PreviousWeekArgs {
}

/** The arguments of `next_week`. */
export interface NextWeekArgs {
}

/** The arguments of `previous_month`. */
export interface PreviousMonthArgs {
}

/** The arguments of `next_month`. */
export interface NextMonthArgs {
}

/** The arguments of `this_week`. */
export interface ThisWeekArgs {
}

/** Attendee — `tl.so.tilo.calendar.Attendee`. */
export interface Attendee extends tilo.Resource {
  oiolhfhy: string;  // name
  zr5l6sfu: string;  // email
  wyfwksgq: string;  // status
  fweteite: string;  // role
}

/** The actions `Attendee` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ATTENDEE_ACTIONS = [] as const;

/** The `tl.so.tilo.calendar.Attendee` kind at `path`. */
export function attendee(ws: tilo.Workspace, path: string): Attendee {
  return resourceProxy(ws, path, SURFACE, ATTENDEE_ACTIONS) as unknown as Attendee;
}

/** Project — `tl.so.tilo.calendar.Project`. */
export interface Project extends tilo.Resource {
  iivr4dfm: string;  // name
  "5kprlx3s": Color;  // color
  fsndgah6: number;  // order
  /** Sets the colour a project is drawn in. */
  set_project_color(args: {
    color: Color;
  }): Promise<void>;
  /** Runs `set_project_color` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_project_colorMany(items: SetProjectColorArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames a project. */
  rename_project(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_projectMany(items: RenameProjectArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Project` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROJECT_ACTIONS = ["set_project_color", "set_project_colorMany", "rename_project", "rename_projectMany"] as const;

/** The `tl.so.tilo.calendar.Project` kind at `path`. */
export function project(ws: tilo.Workspace, path: string): Project {
  return resourceProxy(ws, path, SURFACE, PROJECT_ACTIONS) as unknown as Project;
}

/** Task — `tl.so.tilo.calendar.Task`. */
export interface Task extends tilo.Resource {
  r2vx64eh: string;  // title
  s6raiajy: boolean;  // done
  kibl2yut: string | null;  // due
  "2o4hdoux": string | null;  // remind
  ivfbe5bc: string;  // notes
  whecqzmz: Priority;  // priority
  en7w5zf7: Project | null;  // project
  xna3jvyc: Task | null;  // parent
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
  /** Checks a task off, or opens it again. */
  check_task(args?: {
    done?: boolean;
  }): Promise<void>;
  /** Runs `check_task` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  check_taskMany(items: CheckTaskArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the day a task is due by. */
  due_task(args?: {
    due?: string;
  }): Promise<void>;
  /** Runs `due_task` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  due_taskMany(items: DueTaskArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a task due today. */
  due_task_today(): Promise<void>;
  /** Runs `due_task_today` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  due_task_todayMany(items: DueTaskTodayArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a task due tomorrow. */
  due_task_tomorrow(): Promise<void>;
  /** Runs `due_task_tomorrow` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  due_task_tomorrowMany(items: DueTaskTomorrowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a task due at the start of next week. */
  due_task_next_week(): Promise<void>;
  /** Runs `due_task_next_week` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  due_task_next_weekMany(items: DueTaskNextWeekArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Clears the day a task is due by. */
  due_task_clear(): Promise<void>;
  /** Runs `due_task_clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  due_task_clearMany(items: DueTaskClearArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets how much a task matters. */
  set_task_priority(args: {
    priority: Priority;
  }): Promise<void>;
  /** Runs `set_task_priority` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_task_priorityMany(items: SetTaskPriorityArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Files a task under a project, or under none for the inbox. */
  file_task(args?: {
    p?: Project;
  }): Promise<void>;
  /** Runs `file_task` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  file_taskMany(items: FileTaskArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Task` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TASK_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "check_task", "check_taskMany", "due_task", "due_taskMany", "due_task_today", "due_task_todayMany", "due_task_tomorrow", "due_task_tomorrowMany", "due_task_next_week", "due_task_next_weekMany", "due_task_clear", "due_task_clearMany", "set_task_priority", "set_task_priorityMany", "file_task", "file_taskMany"] as const;

/** The `tl.so.tilo.calendar.Task` kind at `path`. */
export function task(ws: tilo.Workspace, path: string): Task {
  return resourceProxy(ws, path, SURFACE, TASK_ACTIONS) as unknown as Task;
}

/** Event — `tl.so.tilo.calendar.Event`. */
export interface Event extends tilo.Resource {
  qqkexjof: string;  // key
  yclpjnie: string;  // href
  lpmkddba: string;  // etag
  "5b3dmvgx": string;  // title
  f36rh6hq: string;  // start
  ahyl252g: string;  // end
  yyqvqgb4: boolean;  // all_day
  dmlhzgmz: string;  // timezone
  mvt5vjkt: string;  // location
  gqmucsa7: string;  // notes
  igx7zgee: string;  // body
  ypsmw6x6: Array<tilo.Entry<Attendee>>;  // attendees
  ayba7si7: string[];  // categories
  phwonegh: string;  // recurrence
  "6oxxhsol": Repeat;  // repeat
  zjilh6px: number;  // recurrence_every
  uyipmxcj: string | null;  // recurrence_until
  zzjroxh5: number | null;  // recurrence_count
  dn2juryi: string[];  // recurrence_weekdays
  xgv6mdnt: string[];  // exdates
  js6qgkt2: string;  // day
  jz3mybhh: string;  // end_day
  zwcmfzky: string;  // color
  qe2xcyxt: string;  // project
  p2upd4ee: Project | null;  // filed_under
  l44x7h7l: string;  // organizer
  n3y6kpek: boolean;  // cancelled
  "7mqdwjrk": Calendar | null;  // calendar
  "72habrff": string;  // remind
  "2ukd2xzk": Event | null;  // recurrence_of
  x6t7zpds: string;  // original_start
  /** Set one paragraph of a rich-text field to what a Markdown text says. */
  editBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editBlockMany(items: EventEditBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Put a paragraph before the one the address names. */
  insertBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertBlockMany(items: EventInsertBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remove the paragraph the address names, and the paragraphs it holds. */
  deleteBlock(args: {
    block: string;
  }): Promise<void>;
  /** Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteBlockMany(items: EventDeleteBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves an event where it was dropped: a day chip moves to that day keeping its time of day, and an hour moves to the hour it was dropped at. A repeating event waits for its own answer. */
  dropped(args: {
    start: string;
    place: string;
  }): Promise<void>;
  /** Runs `dropped` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  droppedMany(items: DroppedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Ends an event where its edge was dropped. A repeating event waits for its own answer. */
  ended(args: {
    end_at: string;
  }): Promise<void>;
  /** Runs `ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  endedMany(items: EndedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Files an event under a project, or files it under none. */
  file_event(args?: {
    p?: Project;
  }): Promise<void>;
  /** Runs `file_event` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  file_eventMany(items: FileEventArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the colour an event is drawn in. */
  set_event_color(args: {
    color: Color;
  }): Promise<void>;
  /** Runs `set_event_color` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_event_colorMany(items: SetEventColorArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes an event's own colour off: it is drawn in its calendar's. */
  clear_event_color(): Promise<void>;
  /** Runs `clear_event_color` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_event_colorMany(items: ClearEventColorArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the categories an event is filed under in its person's words. */
  set_categories(args: {
    categories: string[];
  }): Promise<void>;
  /** Runs `set_categories` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_categoriesMany(items: SetCategoriesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the zone an event's wall times are read in: `Europe/Berlin`, `UTC`, or `floating` for the same time of day everywhere. */
  set_zone(args: {
    zone: string;
  }): Promise<void>;
  /** Runs `set_zone` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_zoneMany(items: SetZoneArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Files an event under one more category. */
  add_category(args: {
    category: string;
  }): Promise<void>;
  /** Runs `add_category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_categoryMany(items: AddCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a category off an event. */
  remove_category(args: {
    category: string;
  }): Promise<void>;
  /** Runs `remove_category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_categoryMany(items: RemoveCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes someone the organiser of an event. */
  organize(args: {
    who: string;
  }): Promise<void>;
  /** Runs `organize` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  organizeMany(items: OrganizeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reminds the people coming when the event starts. */
  remind_at_start(): Promise<void>;
  /** Runs `remind_at_start` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remind_at_startMany(items: RemindAtStartArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reminds them a given number of minutes before it starts. */
  remind_before(args: {
    minutes: number;
  }): Promise<void>;
  /** Runs `remind_before` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remind_beforeMany(items: RemindBeforeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Tells nobody, once more. */
  remind_off(): Promise<void>;
  /** Runs `remind_off` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remind_offMany(items: RemindOffArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Repeats an event, its rule in iCalendar's words, how often, and where it ends. */
  repeat_rule(args: {
    how: Repeat;
    every?: number;
    weekdays?: string[];
    until?: string;
    count?: number;
  }): Promise<void>;
  /** Runs `repeat_rule` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  repeat_ruleMany(items: RepeatRuleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Repeats an event on chosen days of the week from the day it stands on. */
  repeat_weekdays(args: {
    days: string[];
  }): Promise<void>;
  /** Runs `repeat_weekdays` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  repeat_weekdaysMany(items: RepeatWeekdaysArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops an event repeating from the day it stands on. */
  repeat_off(): Promise<void>;
  /** Runs `repeat_off` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  repeat_offMany(items: RepeatOffArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Skips one date of a repeating event. */
  skip_date(args: {
    day: string;
  }): Promise<void>;
  /** Runs `skip_date` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  skip_dateMany(items: SkipDateArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a skipped date of a repeating event back. */
  restore_date(args: {
    day: string;
  }): Promise<void>;
  /** Runs `restore_date` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  restore_dateMany(items: RestoreDateArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves one date of a repeating event: an event of its own stands in its place and the series keeps every other date. */
  move_occurrence(args: {
    at: string;
    start: string;
    end: string;
  }): Promise<void>;
  /** Runs `move_occurrence` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_occurrenceMany(items: MoveOccurrenceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Cancels one date of a repeating event. */
  cancel_occurrence(args: {
    at: string;
  }): Promise<void>;
  /** Runs `cancel_occurrence` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancel_occurrenceMany(items: CancelOccurrenceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a cancelled or moved date of a series back. */
  restore_occurrence(args: {
    at: string;
  }): Promise<void>;
  /** Runs `restore_occurrence` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  restore_occurrenceMany(items: RestoreOccurrenceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a repeating event from one date on: the series ends the day before, and a new one begins where it was moved to, with every date after it. */
  move_following(args: {
    at: string;
    start: string;
    end: string;
  }): Promise<void>;
  /** Runs `move_following` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_followingMany(items: MoveFollowingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves an event to a day, keeping the time of day it starts and how long it lasts, and its reminder at the same distance from its start. */
  move_to_day(args: {
    day: string;
  }): Promise<void>;
  /** Runs `move_to_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_to_dayMany(items: MoveToDayArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves an event to the day a moment names, keeping the time of day it starts. */
  change_day(args: {
    at: string;
  }): Promise<void>;
  /** Runs `change_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  change_dayMany(items: ChangeDayArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the time of day an event starts, its end keeping how long it lasts. A day the moment names is the day it moves to. */
  change_start(args: {
    at: string;
  }): Promise<void>;
  /** Runs `change_start` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  change_startMany(items: ChangeStartArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets when an event ends. */
  change_end(args: {
    at: string;
  }): Promise<void>;
  /** Runs `change_end` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  change_endMany(items: ChangeEndArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Ends an event on the day a moment names, keeping the time of day it ends at: a day after the one it starts on makes it run through every day between, and a whole-day event ends at the midnight after its last day, which is the day it shows on. */
  move_end_day(args: {
    at: string;
  }): Promise<void>;
  /** Runs `move_end_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_end_dayMany(items: MoveEndDayArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Repeats an event every day, week, month or year from the day it stands on, on the days chosen of a week, and no longer on the days it named before. */
  repeat(args: {
    how: Repeat;
  }): Promise<void>;
  /** Runs `repeat` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  repeatMany(items: RepeatArgs[]): Promise<tilo.ActionAnswer[]>;
  /** How often it repeats, in the unit it repeats in: every two weeks is 2. */
  repeat_every(args: {
    every: number;
  }): Promise<void>;
  /** Runs `repeat_every` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  repeat_everyMany(items: RepeatEveryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The last day it repeats by, or none for a series that does not end. */
  repeat_until(args?: {
    until?: string;
  }): Promise<void>;
  /** Runs `repeat_until` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  repeat_untilMany(items: RepeatUntilArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The last day it repeats by, as the day a moment names. */
  repeat_until_at(args: {
    at: string;
  }): Promise<void>;
  /** Runs `repeat_until_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  repeat_until_atMany(items: RepeatUntilAtArgs[]): Promise<tilo.ActionAnswer[]>;
  /** How many times it repeats in all, or none for a series that does not end. */
  repeat_count(args?: {
    count?: number;
  }): Promise<void>;
  /** Runs `repeat_count` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  repeat_countMany(items: RepeatCountArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Files an event under a calendar the person picks. */
  file_in(args: {
    k: Calendar;
  }): Promise<void>;
  /** Runs `file_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  file_inMany(items: FileInArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Invites someone to an event, by the name and address the person gives. */
  invite(args: {
    name: string;
    email: string;
  }): Promise<Attendee>;
  /** Records someone's answer to an invitation: `accepted`, `declined` or `tentative`. */
  answer(args: {
    g: Attendee;
    status: string;
  }): Promise<void>;
  /** Runs `answer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  answerMany(items: AnswerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes someone off an event's guests: nothing is sent to them. */
  uninvite(args: {
    g: Attendee;
  }): Promise<void>;
  /** Runs `uninvite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  uninviteMany(items: UninviteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a message inviting a guest, the event's calendar invitation carried beside it: what Mail shows the person, to read and send. Nothing is sent by this. */
  invite_by_mail(args: {
    g: Attendee;
  }): Promise<void>;
  /** Runs `invite_by_mail` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  invite_by_mailMany(items: InviteByMailArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Rings an event's reminder when its moment comes, and nothing for an event that has none. */
  ring_due(): Promise<void>;
  /** Runs `ring_due` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  ring_dueMany(items: RingDueArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Tells the people coming that an event starts, once, and clears its reminder. */
  remind_now(): Promise<void>;
  /** Runs `remind_now` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remind_nowMany(items: RemindNowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Goes to the week an event is on. */
  go_to(): Promise<void>;
  /** Runs `go_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  go_toMany(items: GoToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Repeats an event how often the person typed, in the unit it repeats in: 2 for every other. */
  repeat_every_text(args: {
    typed: string;
  }): Promise<void>;
  /** Runs `repeat_every_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  repeat_every_textMany(items: RepeatEveryTextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Repeats an event as many times as the person typed. */
  repeat_count_text(args: {
    typed: string;
  }): Promise<void>;
  /** Runs `repeat_count_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  repeat_count_textMany(items: RepeatCountTextArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Event` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EVENT_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "dropped", "droppedMany", "ended", "endedMany", "file_event", "file_eventMany", "set_event_color", "set_event_colorMany", "clear_event_color", "clear_event_colorMany", "set_categories", "set_categoriesMany", "set_zone", "set_zoneMany", "add_category", "add_categoryMany", "remove_category", "remove_categoryMany", "organize", "organizeMany", "remind_at_start", "remind_at_startMany", "remind_before", "remind_beforeMany", "remind_off", "remind_offMany", "repeat_rule", "repeat_ruleMany", "repeat_weekdays", "repeat_weekdaysMany", "repeat_off", "repeat_offMany", "skip_date", "skip_dateMany", "restore_date", "restore_dateMany", "move_occurrence", "move_occurrenceMany", "cancel_occurrence", "cancel_occurrenceMany", "restore_occurrence", "restore_occurrenceMany", "move_following", "move_followingMany", "move_to_day", "move_to_dayMany", "change_day", "change_dayMany", "change_start", "change_startMany", "change_end", "change_endMany", "move_end_day", "move_end_dayMany", "repeat", "repeatMany", "repeat_every", "repeat_everyMany", "repeat_until", "repeat_untilMany", "repeat_until_at", "repeat_until_atMany", "repeat_count", "repeat_countMany", "file_in", "file_inMany", "invite", "answer", "answerMany", "uninvite", "uninviteMany", "invite_by_mail", "invite_by_mailMany", "ring_due", "ring_dueMany", "remind_now", "remind_nowMany", "go_to", "go_toMany", "repeat_every_text", "repeat_every_textMany", "repeat_count_text", "repeat_count_textMany"] as const;

/** The `tl.so.tilo.calendar.Event` kind at `path`. */
export function calendarEvent(ws: tilo.Workspace, path: string): Event {
  return resourceProxy(ws, path, SURFACE, EVENT_ACTIONS) as unknown as Event;
}

/** Calendar — `tl.so.tilo.calendar.Calendar`. */
export interface Calendar extends tilo.Resource {
  gzrwihvd: string;  // key
  pwrd7qqx: string;  // account
  dn3prfqt: string;  // sync_token
  kc5ls3bi: string;  // name
  kaeu2eyo: string;  // color
  xm42jvvk: boolean;  // writable
  /** Sets the colour a calendar is drawn in. */
  set_calendar_color(args: {
    color: Color;
  }): Promise<void>;
  /** Runs `set_calendar_color` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_calendar_colorMany(items: SetCalendarColorArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames a calendar. */
  rename_calendar(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename_calendar` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_calendarMany(items: RenameCalendarArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Calendar` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CALENDAR_ACTIONS = ["set_calendar_color", "set_calendar_colorMany", "rename_calendar", "rename_calendarMany"] as const;

/** The `tl.so.tilo.calendar.Calendar` kind at `path`. */
export function calendar(ws: tilo.Workspace, path: string): Calendar {
  return resourceProxy(ws, path, SURFACE, CALENDAR_ACTIONS) as unknown as Calendar;
}

/** Calendars — `tl.so.tilo.calendar.Calendars`. */
export interface Calendars extends tilo.Resource {
  g3gq3jmz: Array<tilo.Entry<Event>>;  // events
  ozulkrgt: Array<tilo.Entry<Calendar>>;  // calendars
  jum24lyb: Array<tilo.Entry<Task>>;  // tasks
  japv422q: Array<tilo.Entry<Project>>;  // projects
  i5l2mswn: string;  // anchor_at
  /** Starts an event on a day the person points at, at nine in the morning for an hour, or at the moment the words name. */
  new_event(args?: {
    title?: string;
    day?: string;
    at?: string;
    minutes?: number;
  }): Promise<Event>;
  /** Makes an event of the hours dragged across, from where the drag began to where it ended. */
  new_between(args: {
    start: string;
    end: string;
  }): Promise<Event>;
  /** Adds a task to the inbox or to a project. */
  new_task(args?: {
    p?: Project;
    due?: string;
    title?: string;
  }): Promise<Task>;
  /** Adds a step under a task, in its project, and opens the task if it was done: there is work under it now. */
  add_step(args: {
    t: Task;
    title: string;
  }): Promise<Task>;
  /** Deletes a task and the steps under it. */
  remove_task(args: {
    t: Task;
  }): Promise<void>;
  /** Runs `remove_task` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_taskMany(items: RemoveTaskArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a project. */
  new_project(): Promise<Project>;
  /** Moves a project one place earlier or later among the projects. */
  move_project(args: {
    p: Project;
    up: boolean;
  }): Promise<void>;
  /** Runs `move_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_projectMany(items: MoveProjectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a project; its events and tasks go back to none. */
  remove_project(args: {
    p: Project;
  }): Promise<void>;
  /** Runs `remove_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_projectMany(items: RemoveProjectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stands the window on the day a moment names. */
  go_to_day(args: {
    at: string;
  }): Promise<void>;
  /** Runs `go_to_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  go_to_dayMany(items: GoToDayArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the day before. */
  previous_day(): Promise<void>;
  /** Runs `previous_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  previous_dayMany(items: PreviousDayArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the day after. */
  next_day(): Promise<void>;
  /** Runs `next_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  next_dayMany(items: NextDayArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows today. */
  this_day(): Promise<void>;
  /** Runs `this_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  this_dayMany(items: ThisDayArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks each account's server for its calendars now, so what it holds comes in as it does when the server is heard from. */
  sync(): Promise<void>;
  /** Adds a calendar of the workspace's own. */
  new_calendar(): Promise<Calendar>;
  // the app's `remove` action is the SDK's own: run it with `action("remove")`.
  /** The window shows the week before. */
  previous_week(): Promise<void>;
  /** Runs `previous_week` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  previous_weekMany(items: PreviousWeekArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The window shows the week after. */
  next_week(): Promise<void>;
  /** Runs `next_week` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  next_weekMany(items: NextWeekArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The window shows the month before. */
  previous_month(): Promise<void>;
  /** Runs `previous_month` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  previous_monthMany(items: PreviousMonthArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The window shows the month after. */
  next_month(): Promise<void>;
  /** Runs `next_month` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  next_monthMany(items: NextMonthArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The window shows this week. */
  this_week(): Promise<void>;
  /** Runs `this_week` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  this_weekMany(items: ThisWeekArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The calendar — every event and task it holds, or only one event — as an iCalendar file, for a person to save or an agent to hand on. Nothing is written by this. */
  export_calendar(args?: {
    e?: Event;
  }): Promise<unknown>;
  /** Imports the events and tasks of an iCalendar file, and answers how many it took: what a person's own file offers, and what another calendar exported. A line naming a zone this build cannot read is read in the person's zone. */
  import_ics(args: {
    file: unknown;
  }): Promise<string>;
}

/** The actions `Calendars` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CALENDARS_ACTIONS = ["new_event", "new_between", "new_task", "add_step", "remove_task", "remove_taskMany", "new_project", "move_project", "move_projectMany", "remove_project", "remove_projectMany", "go_to_day", "go_to_dayMany", "previous_day", "previous_dayMany", "next_day", "next_dayMany", "this_day", "this_dayMany", "sync", "new_calendar", "previous_week", "previous_weekMany", "next_week", "next_weekMany", "previous_month", "previous_monthMany", "next_month", "next_monthMany", "this_week", "this_weekMany", "export_calendar", "import_ics"] as const;

/** The `tl.so.tilo.calendar.Calendars` kind at `path`. */
export function calendars(ws: tilo.Workspace, path: string): Calendars {
  return resourceProxy(ws, path, SURFACE, CALENDARS_ACTIONS) as unknown as Calendars;
}

/** Moment — `tl.so.tilo.calendar.Moment`. */
export interface Moment extends tilo.Resource {
  kd3noj7y: Event;  // event
  ejfozpye: string;  // at
  "7hfwgky5": string;  // day
}

/** The actions `Moment` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MOMENT_ACTIONS = [] as const;

/** The `tl.so.tilo.calendar.Moment` kind at `path`. */
export function moment(ws: tilo.Workspace, path: string): Moment {
  return resourceProxy(ws, path, SURFACE, MOMENT_ACTIONS) as unknown as Moment;
}

/** Calendar — `tl.so.tilo.calendar`. */
export interface TiloCalendar extends tilo.Resource {
}

/** The actions `TiloCalendar` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOCALENDAR_ACTIONS = [] as const;

/** The `tl.so.tilo.calendar` kind at `path`. */
export function tiloCalendar(ws: tilo.Workspace, path: string): TiloCalendar {
  return resourceProxy(ws, path, SURFACE, TILOCALENDAR_ACTIONS) as unknown as TiloCalendar;
}

