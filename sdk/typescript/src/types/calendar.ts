/**
 * Types for Calendar (`so.tilo.calendar`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Calendar` is a `Resource` of the app's `calendar.calendar`, made by its factory with the
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
export const SURFACE = "so.tilo.calendar@c920d7ea26877f48";

export interface Attendee {
  id: string;
  name: string;
  email: string;
  status: InviteStatus;
}

export interface AttendeeData {
  id: string;
  name: string;
  email: string;
  status: string;
}

export interface Event {
  id: string;
  title: string;
  start: string;
  end: string;
  end_later_fold: boolean;
  all_day: boolean;
  location: string;
  color: Palette;
  recurrence: Repeat;
  recurrence_interval: number;
  recurrence_until: string | null;
  recurrence_count: number | null;
  recurrence_exdates: string[];
  occurrence_overrides: Array<tilo.Entry<OccurrenceOverride>>;
  timezone: string;
  organizer: string | null;
  attendees: Array<tilo.Entry<Attendee>>;
  reminder_min: number | null;
  project: string | null;
  calendar_id: string | null;
  categories: string[];
}

export interface EventData {
  id: string;
  title: string;
  start: string;
  end: string;
  end_later_fold: boolean;
  all_day: boolean;
  location: string;
  color: number;
  rrule: string;
  recurrence_interval: number;
  recurrence_until: string;
  recurrence_count: number;
  recurrence_exdates: string[];
  occurrence_overrides: OccurrenceOverrideData[];
  timezone: string;
  organizer: string;
  attendees: AttendeeData[];
  reminder: number;
  project: string;
  calendar_id: string;
  categories: string[];
}

/** One of "NeedsAction", "Accepted", "Tentative", "Declined". */
export type InviteStatus = "NeedsAction" | "Accepted" | "Tentative" | "Declined";

export interface OccurrenceOverride {
  original_date: string;
  original_time: string | null;
  cancelled: boolean;
  start: string | null;
  end: string | null;
  end_later_fold: boolean;
  title: string | null;
  location: string | null;
}

export interface OccurrenceOverrideData {
  original_date: string;
  original_time: string;
  cancelled: boolean;
  start: string;
  end: string;
  end_later_fold: boolean;
  title: string;
  location: string;
}

/** One of "indigo", "emerald", "amber", "red", "blue", "violet", "slate". */
export type Palette = "indigo" | "emerald" | "amber" | "red" | "blue" | "violet" | "slate";

/** One of "None", "Low", "Medium", "High". */
export type Priority = "None" | "Low" | "Medium" | "High";

export interface Project {
  id: string;
  name: string;
  color: Palette;
  order: number;
}

export interface RemoteAt {
  calendar: string;
  href: string;
  etag: string | null;
}

export interface RemoteCopy {
  calendar: string;
  href: string;
  etag: string | null;
  holds: string;
  task: boolean;
}

export interface Removal {
  item: string;
  calendar: string;
  href: string;
  etag: string | null;
}

/** One of "None", "Daily", "Weekdays", "Weekly", "Monthly", "Yearly". */
export type Repeat = "None" | "Daily" | "Weekdays" | "Weekly" | "Monthly" | "Yearly";

export interface Task {
  id: string;
  title: string;
  done: boolean;
  due: string | null;
  priority: Priority;
  project: string | null;
  parent: string | null;
}

export interface TaskData {
  id: string;
  title: string;
  done: boolean;
  due: string;
  priority: number;
  project: string;
  parent: string;
}

export interface Upload {
  event: string;
  calendar: string;
}

export interface ListedAccount {
  type_id: string;
  display_name: string;
  config: unknown;
  enabled: boolean;
  tint: number;
}

export interface Endpoint {
  service: string;
  recipient: string;
  scope: string;
  revision: string;
  removed: boolean;
}

export interface Person {
  label: string;
  removed: boolean;
  phones: string[];
  emails: string[];
  photo: string;
  endpoints: Array<tilo.Entry<Endpoint>>;
}

export interface Projected {
  person: string;
  label: string;
  phones: string[];
  emails: string[];
  photo: string;
}

/** The arguments of `new-event`. */
export interface NewEventArgs {
  event: EventData;
}

/** The arguments of `publish-browser-identity`. */
export interface PublishBrowserIdentityArgs {
  root: string;
  origin: string;
  display_name: string;
}

/** The arguments of `save-account`. */
export interface SaveAccountArgs {
  id: string;
  type_id: string;
  display_name: string;
  config: unknown;
  enabled: boolean;
  tint: number;
}

/** The arguments of `remove-account`. */
export interface RemoveAccountArgs {
  account: string;
}

/** The arguments of `project-contact`. */
export interface ProjectContactArgs {
  person: string;
  label: string;
}

/** The arguments of `remove-contact`. */
export interface RemoveContactArgs {
  person: string;
}

/** The arguments of `confirm-endpoint`. */
export interface ConfirmEndpointArgs {
  person: string;
  endpoint: string;
  service: string;
  recipient: string;
}

/** The arguments of `confirm-account`. */
export interface ConfirmAccountArgs {
  person: string;
  endpoint: string;
  service: string;
  scope: string;
  recipient: string;
  revision: string;
}

/** The arguments of `replace-endpoint`. */
export interface ReplaceEndpointArgs {
  person: string;
  previous: string;
  endpoint: string;
  service: string;
  recipient: string;
}

/** The arguments of `replace-account`. */
export interface ReplaceAccountArgs {
  person: string;
  previous: string;
  endpoint: string;
  service: string;
  scope: string;
  recipient: string;
  revision: string;
}

/** The arguments of `project-phones`. */
export interface ProjectPhonesArgs {
  person: string;
  phones: string[];
}

/** The arguments of `project-emails`. */
export interface ProjectEmailsArgs {
  person: string;
  emails: string[];
}

/** The arguments of `project-contacts`. */
export interface ProjectContactsArgs {
  people: Projected[];
  removed: string[];
}

/** The arguments of `forget-endpoint`. */
export interface ForgetEndpointArgs {
  person: string;
  endpoint: string;
}

/** Calendar — `calendar.calendar`. */
export interface Calendar extends tilo.Resource {
  projects: Array<tilo.Entry<Project>>;
  events: Array<tilo.Entry<Event>>;
  tasks: Array<tilo.Entry<Task>>;
  notes: Record<string, string>;
  readonly remote: Record<string, RemoteCopy>;
  readonly sync_tokens: Record<string, string>;
  newEvent(args: {
    event: EventData;
  }): Promise<void>;
  /** Runs `new-event` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  newEventMany(items: NewEventArgs[]): Promise<tilo.ActionAnswer[]>;
  newTask(args: {
    task: TaskData;
  }): Promise<void>;
  newProject(args: {
    name: string;
  }): Promise<void>;
  deleteEvent(args: {
    event: string;
  }): Promise<void>;
  reschedule(args: {
    event: string;
    start: string;
    end: string;
  }): Promise<void>;
  setStart(args: {
    event: string;
    start: string;
  }): Promise<void>;
  moveToCalendar(args: {
    event: string;
    color: Palette;
    calendar?: string;
  }): Promise<void>;
  invite(args: {
    event: string;
    email: string;
    name: string;
  }): Promise<void>;
  setCategories(args: {
    event: string;
    categories: string[];
  }): Promise<void>;
  addInvitation(args: {
    ics: string;
  }): Promise<unknown>;
  deleteTask(args: {
    task: string;
  }): Promise<void>;
  deleteProject(args: {
    project: string;
  }): Promise<void>;
  importItems(args: {
    events: EventData[];
    tasks: TaskData[];
  }): Promise<void>;
  adoptRemoteEvent(args: {
    event: EventData;
    at: RemoteAt;
  }): Promise<void>;
  adoptRemoteTask(args: {
    task: TaskData;
    at: RemoteAt;
  }): Promise<void>;
  dropRemoteItem(args: {
    item: string;
  }): Promise<void>;
  serverApplied(args: {
    event: string;
    at: RemoteAt;
    holds: string;
  }): Promise<void>;
  serverRemoved(args: {
    item: string;
  }): Promise<void>;
  pulled(args: {
    calendar: string;
    token: string;
  }): Promise<void>;
  exportIcsContent(args?: {
    event?: string;
  }): Promise<void>;
}

/** The actions `Calendar` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CALENDAR_ACTIONS = ["newEvent", "newEventMany", "newTask", "newProject", "deleteEvent", "reschedule", "setStart", "moveToCalendar", "invite", "setCategories", "addInvitation", "deleteTask", "deleteProject", "importItems", "adoptRemoteEvent", "adoptRemoteTask", "dropRemoteItem", "serverApplied", "serverRemoved", "pulled", "exportIcsContent"] as const;

/** The `calendar.calendar` kind at `path`. */
export function calendar(ws: tilo.Workspace, path: string): Calendar {
  return resourceProxy(ws, path, SURFACE, CALENDAR_ACTIONS) as unknown as Calendar;
}

/** Accounts — `tilo.accounts`. */
export interface Accounts extends tilo.Resource {
  accounts: Array<tilo.Entry<ListedAccount>>;
  publishBrowserIdentity(args: {
    root: string;
    origin: string;
    display_name: string;
  }): Promise<void>;
  /** Runs `publish-browser-identity` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  publishBrowserIdentityMany(items: PublishBrowserIdentityArgs[]): Promise<tilo.ActionAnswer[]>;
  saveAccount(args: {
    id: string;
    type_id: string;
    display_name: string;
    config: unknown;
    enabled: boolean;
    tint: number;
  }): Promise<void>;
  /** Runs `save-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  saveAccountMany(items: SaveAccountArgs[]): Promise<tilo.ActionAnswer[]>;
  removeAccount(args: {
    account: string;
  }): Promise<void>;
  /** Runs `remove-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeAccountMany(items: RemoveAccountArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Accounts` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACCOUNTS_ACTIONS = ["publishBrowserIdentity", "publishBrowserIdentityMany", "saveAccount", "saveAccountMany", "removeAccount", "removeAccountMany"] as const;

/** The `tilo.accounts` kind at `path`. */
export function accounts(ws: tilo.Workspace, path: string): Accounts {
  return resourceProxy(ws, path, SURFACE, ACCOUNTS_ACTIONS) as unknown as Accounts;
}

/** Contact identities — `tilo.contacts.identities`. */
export interface Identities extends tilo.Resource {
  people: Array<tilo.Entry<Person>>;
  projectContact(args: {
    person: string;
    label: string;
  }): Promise<void>;
  /** Runs `project-contact` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  projectContactMany(items: ProjectContactArgs[]): Promise<tilo.ActionAnswer[]>;
  removeContact(args: {
    person: string;
  }): Promise<void>;
  /** Runs `remove-contact` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeContactMany(items: RemoveContactArgs[]): Promise<tilo.ActionAnswer[]>;
  confirmEndpoint(args: {
    person: string;
    endpoint: string;
    service: string;
    recipient: string;
  }): Promise<void>;
  /** Runs `confirm-endpoint` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  confirmEndpointMany(items: ConfirmEndpointArgs[]): Promise<tilo.ActionAnswer[]>;
  confirmAccount(args: {
    person: string;
    endpoint: string;
    service: string;
    scope: string;
    recipient: string;
    revision: string;
  }): Promise<void>;
  /** Runs `confirm-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  confirmAccountMany(items: ConfirmAccountArgs[]): Promise<tilo.ActionAnswer[]>;
  replaceEndpoint(args: {
    person: string;
    previous: string;
    endpoint: string;
    service: string;
    recipient: string;
  }): Promise<void>;
  /** Runs `replace-endpoint` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replaceEndpointMany(items: ReplaceEndpointArgs[]): Promise<tilo.ActionAnswer[]>;
  replaceAccount(args: {
    person: string;
    previous: string;
    endpoint: string;
    service: string;
    scope: string;
    recipient: string;
    revision: string;
  }): Promise<void>;
  /** Runs `replace-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replaceAccountMany(items: ReplaceAccountArgs[]): Promise<tilo.ActionAnswer[]>;
  projectPhones(args: {
    person: string;
    phones: string[];
  }): Promise<void>;
  /** Runs `project-phones` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  projectPhonesMany(items: ProjectPhonesArgs[]): Promise<tilo.ActionAnswer[]>;
  projectEmails(args: {
    person: string;
    emails: string[];
  }): Promise<void>;
  /** Runs `project-emails` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  projectEmailsMany(items: ProjectEmailsArgs[]): Promise<tilo.ActionAnswer[]>;
  projectContacts(args: {
    people: Projected[];
    removed: string[];
  }): Promise<void>;
  /** Runs `project-contacts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  projectContactsMany(items: ProjectContactsArgs[]): Promise<tilo.ActionAnswer[]>;
  forgetEndpoint(args: {
    person: string;
    endpoint: string;
  }): Promise<void>;
  /** Runs `forget-endpoint` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetEndpointMany(items: ForgetEndpointArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Identities` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const IDENTITIES_ACTIONS = ["projectContact", "projectContactMany", "removeContact", "removeContactMany", "confirmEndpoint", "confirmEndpointMany", "confirmAccount", "confirmAccountMany", "replaceEndpoint", "replaceEndpointMany", "replaceAccount", "replaceAccountMany", "projectPhones", "projectPhonesMany", "projectEmails", "projectEmailsMany", "projectContacts", "projectContactsMany", "forgetEndpoint", "forgetEndpointMany"] as const;

/** The `tilo.contacts.identities` kind at `path`. */
export function identities(ws: tilo.Workspace, path: string): Identities {
  return resourceProxy(ws, path, SURFACE, IDENTITIES_ACTIONS) as unknown as Identities;
}

