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
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.calendar@0489bf8990ba967c";

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
  occurrence_overrides: OccurrenceOverride[];
  timezone: string;
  organizer: string | null;
  attendees: Attendee[];
  reminder_min: number | null;
  project: string | null;
  calendar_id: string | null;
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
}

/** One of "needs_action", "accepted", "tentative", "declined". */
export type InviteStatus = "needs_action" | "accepted" | "tentative" | "declined";

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

/** One of "none", "low", "medium", "high". */
export type Priority = "none" | "low" | "medium" | "high";

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

/** One of "none", "daily", "weekdays", "weekly", "monthly", "yearly". */
export type Repeat = "none" | "daily" | "weekdays" | "weekly" | "monthly" | "yearly";

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

export interface PersonAccount {
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
  endpoints: Endpoint[];
}

export interface Projected {
  person: string;
  label: string;
  phones: string[];
  emails: string[];
  photo: string;
}

/** Calendar — `calendar.calendar`. */
export interface Calendar extends tilo.Resource {
  projects: Project[];
  events: Event[];
  tasks: Task[];
  notes: Record<string, string>;
  readonly remote: Record<string, RemoteCopy>;
  readonly sync_tokens: Record<string, string>;
  newEvent(args: {
    event: EventData;
  }): Promise<tilo.Run | void>;
  newTask(args: {
    task: TaskData;
  }): Promise<tilo.Run | void>;
  newProject(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  deleteEvent(args: {
    event: string;
  }): Promise<tilo.Run | void>;
  reschedule(args: {
    event: string;
    start: string;
    end: string;
  }): Promise<tilo.Run | void>;
  setStart(args: {
    event: string;
    start: string;
  }): Promise<tilo.Run | void>;
  moveToCalendar(args: {
    event: string;
    color: Palette;
    calendar?: string;
  }): Promise<tilo.Run | void>;
  invite(args: {
    event: string;
    email: string;
    name: string;
  }): Promise<tilo.Run | void>;
  deleteTask(args: {
    task: string;
  }): Promise<tilo.Run | void>;
  deleteProject(args: {
    project: string;
  }): Promise<tilo.Run | void>;
  importItems(args: {
    events: EventData[];
    tasks: TaskData[];
  }): Promise<tilo.Run | void>;
  adoptRemoteEvent(args: {
    event: EventData;
    at: RemoteAt;
  }): Promise<tilo.Run | void>;
  adoptRemoteTask(args: {
    task: TaskData;
    at: RemoteAt;
  }): Promise<tilo.Run | void>;
  dropRemoteItem(args: {
    item: string;
  }): Promise<tilo.Run | void>;
  serverApplied(args: {
    event: string;
    at: RemoteAt;
    holds: string;
  }): Promise<tilo.Run | void>;
  serverRemoved(args: {
    item: string;
  }): Promise<tilo.Run | void>;
  pulled(args: {
    calendar: string;
    token: string;
  }): Promise<tilo.Run | void>;
  exportIcsContent(args?: {
    event?: string;
  }): Promise<void>;
}

/** The `calendar.calendar` kind at `path`. */
export function calendar(ws: tilo.Workspace, path: string): Calendar {
  return resourceProxy(ws, path, SURFACE) as unknown as Calendar;
}

/** Accounts — `tilo.accounts`. */
export interface Accounts extends tilo.Resource {
  accounts: PersonAccount[];
  active: Record<string, string>;
  publishBrowserIdentity(args: {
    root: string;
    origin: string;
    display_name: string;
  }): Promise<tilo.Run | void>;
  saveAccount(args: {
    id: string;
    type_id: string;
    display_name: string;
    config: unknown;
    enabled: boolean;
    tint: number;
  }): Promise<tilo.Run | void>;
  removeAccount(args: {
    account: string;
  }): Promise<tilo.Run | void>;
  setActiveAccount(args: {
    account: string;
  }): Promise<tilo.Run | void>;
}

/** The `tilo.accounts` kind at `path`. */
export function accounts(ws: tilo.Workspace, path: string): Accounts {
  return resourceProxy(ws, path, SURFACE) as unknown as Accounts;
}

/** Contact identities — `tilo.contacts.identities`. */
export interface Identities extends tilo.Resource {
  people: Person[];
  projectContact(args: {
    person: string;
    label: string;
  }): Promise<tilo.Run | void>;
  removeContact(args: {
    person: string;
  }): Promise<tilo.Run | void>;
  confirmEndpoint(args: {
    person: string;
    endpoint: string;
    service: string;
    recipient: string;
  }): Promise<tilo.Run | void>;
  confirmAccount(args: {
    person: string;
    endpoint: string;
    service: string;
    scope: string;
    recipient: string;
    revision: string;
  }): Promise<tilo.Run | void>;
  replaceEndpoint(args: {
    person: string;
    previous: string;
    endpoint: string;
    service: string;
    recipient: string;
  }): Promise<tilo.Run | void>;
  replaceAccount(args: {
    person: string;
    previous: string;
    endpoint: string;
    service: string;
    scope: string;
    recipient: string;
    revision: string;
  }): Promise<tilo.Run | void>;
  projectPhones(args: {
    person: string;
    phones: string[];
  }): Promise<tilo.Run | void>;
  projectEmails(args: {
    person: string;
    emails: string[];
  }): Promise<tilo.Run | void>;
  projectContacts(args: {
    people: Projected[];
    removed: string[];
  }): Promise<tilo.Run | void>;
  forgetEndpoint(args: {
    person: string;
    endpoint: string;
  }): Promise<tilo.Run | void>;
}

/** The `tilo.contacts.identities` kind at `path`. */
export function identities(ws: tilo.Workspace, path: string): Identities {
  return resourceProxy(ws, path, SURFACE) as unknown as Identities;
}

