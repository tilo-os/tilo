/**
 * Types for Calendar (`so.tilo.calendar`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
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
export const SURFACE = "so.tilo.calendar@d74fb1f7da382914";

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

/** The arguments of `file_event`. */
export interface FileEventArgs {
  p?: Project;
}

/** The arguments of `invite_by_mail`. */
export interface InviteByMailArgs {
  g: Attendee;
}

/** The arguments of `go_to`. */
export interface GoToArgs {
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

/** Account — `tl.so.tilo.calendar.Account`. */
export interface Account extends tilo.Resource {
  f44j5f26: string;  // key
  gxn236ef: string;  // address
  rfeeyuff: string;  // name
  turzovgi: string;  // server
  vupfntcd: boolean;  // read_only
  cgmndfzg: string;  // trouble
  yfntdxb7: boolean;  // online
  "5ebgjcbp": boolean;  // checking
  qmpjl3oc: string;  // fetched
  /** Asks one account's server for its calendars now: what 'Sync again' over its notice does. */
  sync_account(): Promise<void>;
}

/** The actions `Account` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACCOUNT_ACTIONS = ["sync_account"] as const;

/** The `tl.so.tilo.calendar.Account` kind at `path`. */
export function account(ws: tilo.Workspace, path: string): Account {
  return resourceProxy(ws, path, SURFACE, ACCOUNT_ACTIONS) as unknown as Account;
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
  /** Ends an event where its edge was dropped. A repeating event waits for its own answer. */
  ended(args: {
    end_at: string;
  }): Promise<void>;
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
  /** Takes an event's own colour off: it is drawn in its calendar's. */
  clear_event_color(): Promise<void>;
  /** Sets the categories an event is filed under in its person's words. */
  set_categories(args: {
    categories: string[];
  }): Promise<void>;
  /** Sets the zone an event's wall times are read in: `Europe/Berlin`, `UTC`, or `floating` for the same time of day everywhere. */
  set_zone(args: {
    zone: string;
  }): Promise<void>;
  /** Files an event under one more category. */
  add_category(args: {
    category: string;
  }): Promise<void>;
  /** Takes a category off an event. */
  remove_category(args: {
    category: string;
  }): Promise<void>;
  /** Makes someone the organiser of an event. */
  organize(args: {
    who: string;
  }): Promise<void>;
  /** Reminds the people coming when the event starts. */
  remind_at_start(): Promise<void>;
  /** Reminds them a given number of minutes before it starts. */
  remind_before(args: {
    minutes: number;
  }): Promise<void>;
  /** Tells nobody, once more. */
  remind_off(): Promise<void>;
  /** Repeats an event, its rule in iCalendar's words, how often, and where it ends. */
  repeat_rule(args: {
    how: Repeat;
    every?: number;
    weekdays?: string[];
    until?: string;
    count?: number;
  }): Promise<void>;
  /** Repeats an event on chosen days of the week from the day it stands on. */
  repeat_weekdays(args: {
    days: string[];
  }): Promise<void>;
  /** Adds a weekday to a weekly repeat, or takes it off: the days chosen are the rule's `BYDAY`, and the repeat keeps the day it ends on, if it ends on one. */
  toggle_weekday(args: {
    day: string;
  }): Promise<void>;
  /** Stops an event repeating from the day it stands on. */
  repeat_off(): Promise<void>;
  /** Skips one date of a repeating event. */
  skip_date(args: {
    day: string;
  }): Promise<void>;
  /** Gives a skipped date of a repeating event back. */
  restore_date(args: {
    day: string;
  }): Promise<void>;
  /** Moves one date of a repeating event: an event of its own stands in its place and the series keeps every other date. */
  move_occurrence(args: {
    at: string;
    start: string;
    end: string;
  }): Promise<void>;
  /** Cancels one date of a repeating event. */
  cancel_occurrence(args: {
    at: string;
  }): Promise<void>;
  /** Gives a cancelled or moved date of a series back. */
  restore_occurrence(args: {
    at: string;
  }): Promise<void>;
  /** Moves a repeating event from one date on: the series ends the day before, and a new one begins where it was moved to, with every date after it. */
  move_following(args: {
    at: string;
    start: string;
    end: string;
  }): Promise<void>;
  /** Moves an event to a day, keeping the time of day it starts and how long it lasts, and its reminder at the same distance from its start. */
  move_to_day(args: {
    day: string;
  }): Promise<void>;
  /** Moves an event to the day a moment names, keeping the time of day it starts. */
  change_day(args: {
    at: string;
  }): Promise<void>;
  /** Sets the time of day an event starts, its end keeping how long it lasts. A day the moment names is the day it moves to. */
  change_start(args: {
    at: string;
  }): Promise<void>;
  /** Sets when an event ends. */
  change_end(args: {
    at: string;
  }): Promise<void>;
  /** Ends an event on the day a moment names, keeping the time of day it ends at: a day after the one it starts on makes it run through every day between, and a whole-day event ends at the midnight after its last day, which is the day it shows on. */
  move_end_day(args: {
    at: string;
  }): Promise<void>;
  /** Ends an event on a day, keeping the time of day it ends at, as `move_end_day` does with the day a moment names. */
  end_on_day(args: {
    end_day: string;
  }): Promise<void>;
  /** Repeats an event every day, week, month or year from the day it stands on, on the days chosen of a week, and no longer on the days it named before. */
  repeat(args: {
    how: Repeat;
  }): Promise<void>;
  /** How often it repeats, in the unit it repeats in: every two weeks is 2. */
  repeat_every(args: {
    every: number;
  }): Promise<void>;
  /** The last day it repeats by, or none for a series that does not end. */
  repeat_until(args?: {
    until?: string;
  }): Promise<void>;
  /** The last day it repeats by, as the day a moment names. */
  repeat_until_at(args: {
    at: string;
  }): Promise<void>;
  /** How many times it repeats in all, or none for a series that does not end. */
  repeat_count(args?: {
    count?: number;
  }): Promise<void>;
  /** Files an event under a calendar the person picks. */
  file_in(args: {
    k: Calendar;
  }): Promise<void>;
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
  /** Answers the person's own invitation to an account's event, telling the organiser through the account's server: what settles the RSVP the invitation asked for. */
  answer_and_tell(args: {
    g: Attendee;
    said: string;
  }): Promise<void>;
  /** Takes someone off an event's guests: nothing is sent to them. */
  uninvite(args: {
    g: Attendee;
  }): Promise<void>;
  /** Starts a message inviting a guest, the event's calendar invitation carried beside it: what Mail shows the person, to read and send. Nothing is sent by this. */
  invite_by_mail(args: {
    g: Attendee;
  }): Promise<void>;
  /** Runs `invite_by_mail` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  invite_by_mailMany(items: InviteByMailArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Rings an event's reminder when its moment comes, and nothing for an event that has none. */
  ring_due(): Promise<void>;
  /** Tells the people coming that an event starts, once, and clears its reminder. */
  remind_now(): Promise<void>;
  /** Goes to the week an event is on. */
  go_to(): Promise<void>;
  /** Runs `go_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  go_toMany(items: GoToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Repeats an event how often the person typed, in the unit it repeats in: 2 for every other. */
  repeat_every_text(args: {
    typed: string;
  }): Promise<void>;
  /** Repeats an event as many times as the person typed. */
  repeat_count_text(args: {
    typed: string;
  }): Promise<void>;
}

/** The actions `Event` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EVENT_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "dropped", "ended", "file_event", "file_eventMany", "set_event_color", "clear_event_color", "set_categories", "set_zone", "add_category", "remove_category", "organize", "remind_at_start", "remind_before", "remind_off", "repeat_rule", "repeat_weekdays", "toggle_weekday", "repeat_off", "skip_date", "restore_date", "move_occurrence", "cancel_occurrence", "restore_occurrence", "move_following", "move_to_day", "change_day", "change_start", "change_end", "move_end_day", "end_on_day", "repeat", "repeat_every", "repeat_until", "repeat_until_at", "repeat_count", "file_in", "invite", "answer", "answer_and_tell", "uninvite", "invite_by_mail", "invite_by_mailMany", "ring_due", "remind_now", "go_to", "go_toMany", "repeat_every_text", "repeat_count_text"] as const;

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
  /** Renames a calendar. */
  rename_calendar(args: {
    name: string;
  }): Promise<void>;
}

/** The actions `Calendar` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CALENDAR_ACTIONS = ["set_calendar_color", "rename_calendar"] as const;

/** The `tl.so.tilo.calendar.Calendar` kind at `path`. */
export function calendar(ws: tilo.Workspace, path: string): Calendar {
  return resourceProxy(ws, path, SURFACE, CALENDAR_ACTIONS) as unknown as Calendar;
}

/** Calendars — `tl.so.tilo.calendar.Calendars`. */
export interface Calendars extends tilo.Resource {
  g3gq3jmz: Array<tilo.Entry<Event>>;  // events
  ozulkrgt: Array<tilo.Entry<Calendar>>;  // calendars
  lubytvuv: Array<tilo.Entry<Account>>;  // accounts
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
  /** Throws away an event nothing was written in yet: what a person's New event leaves when they decide against it. */
  discard_draft(args: {
    e: Event;
  }): Promise<void>;
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
  /** The calendar — every event and task it holds, or only one event — as an iCalendar file, for a person to save or an agent to hand on. Nothing is written by this. The system's one Export… lists it, and saves the file where the person says. */
  export_calendar(args?: {
    e?: Event;
  }): Promise<unknown>;
  /** Takes an invitation into the calendar: the event it asks for is kept by the account's server where one can be written to it, so the organiser hears the answer, and by this workspace otherwise. What the server took comes back with the account's next sync. */
  take_invitation(args: {
    text: string;
  }): Promise<string>;
  /** Imports the events and tasks of an iCalendar file, and answers how many it took: what a person's own file offers, and what another calendar exported. A line naming a zone this build cannot read is read in the person's zone. */
  import_ics(args: {
    file: unknown;
  }): Promise<string>;
}

/** The actions `Calendars` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CALENDARS_ACTIONS = ["new_event", "new_between", "new_task", "add_step", "remove_task", "remove_taskMany", "new_project", "move_project", "move_projectMany", "remove_project", "remove_projectMany", "go_to_day", "go_to_dayMany", "previous_day", "previous_dayMany", "next_day", "next_dayMany", "this_day", "this_dayMany", "sync", "new_calendar", "discard_draft", "previous_week", "previous_weekMany", "next_week", "next_weekMany", "previous_month", "previous_monthMany", "next_month", "next_monthMany", "this_week", "this_weekMany", "export_calendar", "take_invitation", "import_ics"] as const;

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

