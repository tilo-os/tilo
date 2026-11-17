/**
 * Types for Maps (`so.tilo.maps`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Workspace` is a `Resource` of the app's `maps.workspace`, made by its factory with the
 * resource's path (`maps/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.maps@bff2580c0be19545";

/** One of "landmark", "cafe", "restaurant", "bar", "museum", "park", "transit", "hotel", "shop", "hospital", "other". */
export type Category = "landmark" | "cafe" | "restaurant" | "bar" | "museum" | "park" | "transit" | "hotel" | "shop" | "hospital" | "other";

export interface LatLon {
  lat: number;
  lon: number;
}

/** One of "Depart", "Continue", "SlightLeft", "Left", "SharpLeft", "SlightRight", "Right", "SharpRight", "Uturn", "Roundabout", "Merge", "Arrive". */
export type Maneuver = "Depart" | "Continue" | "SlightLeft" | "Left" | "SharpLeft" | "SlightRight" | "Right" | "SharpRight" | "Uturn" | "Roundabout" | "Merge" | "Arrive";

export interface Place {
  id: string;
  name: string;
  category: Category;
  address: string;
  pos: LatLon;
  saved: boolean;
}

export interface Route {
  mode: TravelMode;
  distance_m: number;
  duration_s: number;
  geometry: LatLon[];
  steps: RouteStep[];
  from: LatLon;
  to: LatLon;
}

export interface RouteStep {
  instruction: string;
  distance_m: number;
  maneuver: Maneuver;
}

export interface TransitJourney {
  dep: string;
  arr: string;
  duration_min: number;
  transfers: number;
  legs: TransitLeg[];
}

export interface TransitLeg {
  mode: TransitMode;
  line: string;
  headsign: string;
  from_name: string;
  to_name: string;
  dep: string;
  arr: string;
  duration_min: number;
  num_stops: number;
  geometry: LatLon[];
  color: string | null;
  text_color: string | null;
  agency: string;
  realtime: boolean;
}

/** One of "Walk", "Bus", "Tram", "Subway", "Rail", "Ferry". */
export type TransitMode = "Walk" | "Bus" | "Tram" | "Subway" | "Rail" | "Ferry";

/** One of "car", "transit", "bike", "foot". */
export type TravelMode = "car" | "transit" | "bike" | "foot";

export interface DownloadedRegion {
  id: string;
  name: string;
  min: LatLon;
  max: LatLon;
  z0: number;
  z1: number;
  tiles: number;
}

/** The arguments of `accept-places`. */
export interface AcceptPlacesArgs {
  places: Place[];
}

/** The arguments of `commit-search`. */
export interface CommitSearchArgs {
  query: string;
}

/** The arguments of `clear-recents`. */
export interface ClearRecentsArgs {
}

/** The arguments of `accept-route`. */
export interface AcceptRouteArgs {
  route: Route;
  origin_name: string;
  dest_name: string;
  mode: TravelMode;
  origin?: LatLon;
  dest?: LatLon;
}

/** The arguments of `clear-route`. */
export interface ClearRouteArgs {
}

/** The arguments of `accept-transit`. */
export interface AcceptTransitArgs {
  transit: TransitJourney[];
  origin_name: string;
  dest_name: string;
  origin?: LatLon;
  dest?: LatLon;
}

/** The arguments of `clear-transit`. */
export interface ClearTransitArgs {
}

/** The arguments of `record`. */
export interface RecordArgs {
  device: string;
  region: DownloadedRegion;
}

/** The arguments of `forget`. */
export interface ForgetArgs {
  device: string;
  area: string;
}

/** The arguments of `clear`. */
export interface ClearArgs {
  device: string;
}

/** Maps — `maps.workspace`. */
export interface Workspace extends tilo.Resource {
  places: Array<tilo.Entry<Place>>;
  readonly recents: unknown;
  mode: TravelMode;
  transit_time: unknown;
  readonly route_plan: unknown;
  readonly transit_plan: unknown;
  acceptPlaces(args: {
    places: Place[];
  }): Promise<void>;
  /** Runs `accept-places` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  acceptPlacesMany(items: AcceptPlacesArgs[]): Promise<tilo.ActionAnswer[]>;
  commitSearch(args: {
    query: string;
  }): Promise<void>;
  /** Runs `commit-search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  commitSearchMany(items: CommitSearchArgs[]): Promise<tilo.ActionAnswer[]>;
  clearRecents(): Promise<void>;
  /** Runs `clear-recents` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clearRecentsMany(items: ClearRecentsArgs[]): Promise<tilo.ActionAnswer[]>;
  acceptRoute(args: {
    route: Route;
    origin_name: string;
    dest_name: string;
    mode: TravelMode;
    origin?: LatLon;
    dest?: LatLon;
  }): Promise<void>;
  /** Runs `accept-route` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  acceptRouteMany(items: AcceptRouteArgs[]): Promise<tilo.ActionAnswer[]>;
  clearRoute(): Promise<void>;
  /** Runs `clear-route` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clearRouteMany(items: ClearRouteArgs[]): Promise<tilo.ActionAnswer[]>;
  acceptTransit(args: {
    transit: TransitJourney[];
    origin_name: string;
    dest_name: string;
    origin?: LatLon;
    dest?: LatLon;
  }): Promise<void>;
  /** Runs `accept-transit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  acceptTransitMany(items: AcceptTransitArgs[]): Promise<tilo.ActionAnswer[]>;
  clearTransit(): Promise<void>;
  /** Runs `clear-transit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clearTransitMany(items: ClearTransitArgs[]): Promise<tilo.ActionAnswer[]>;
  search(args: {
    query: string;
  }): Promise<void>;
  locate(): Promise<void>;
  toggleTraffic(): Promise<void>;
  openPlace(args: {
    place: string;
  }): Promise<void>;
  directions(args: {
    place: string;
  }): Promise<void>;
  openPoi(args: {
    poi: string;
  }): Promise<void>;
  poiDirections(args: {
    poi: string;
  }): Promise<void>;
  showPoint(args: {
    lat: number;
    lon: number;
  }): Promise<void>;
  deleteOfflineArea(args: {
    area: string;
  }): Promise<void>;
  clearOfflineAreas(): Promise<void>;
}

/** The actions `Workspace` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKSPACE_ACTIONS = ["acceptPlaces", "acceptPlacesMany", "commitSearch", "commitSearchMany", "clearRecents", "clearRecentsMany", "acceptRoute", "acceptRouteMany", "clearRoute", "clearRouteMany", "acceptTransit", "acceptTransitMany", "clearTransit", "clearTransitMany", "search", "locate", "toggleTraffic", "openPlace", "directions", "openPoi", "poiDirections", "showPoint", "deleteOfflineArea", "clearOfflineAreas"] as const;

/** The `maps.workspace` kind at `path`. */
export function workspace(ws: tilo.Workspace, path: string): Workspace {
  return resourceProxy(ws, path, SURFACE, WORKSPACE_ACTIONS) as unknown as Workspace;
}

/** Offline areas — `maps.offline-areas`. */
export interface OfflineAreas extends tilo.Resource {
  devices: Record<string, unknown>;
  record(args: {
    device: string;
    region: DownloadedRegion;
  }): Promise<void>;
  /** Runs `record` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordMany(items: RecordArgs[]): Promise<tilo.ActionAnswer[]>;
  forget(args: {
    device: string;
    area: string;
  }): Promise<void>;
  /** Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetMany(items: ForgetArgs[]): Promise<tilo.ActionAnswer[]>;
  clear(args: {
    device: string;
  }): Promise<void>;
  /** Runs `clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clearMany(items: ClearArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `OfflineAreas` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OFFLINEAREAS_ACTIONS = ["record", "recordMany", "forget", "forgetMany", "clear", "clearMany"] as const;

/** The `maps.offline-areas` kind at `path`. */
export function offlineAreas(ws: tilo.Workspace, path: string): OfflineAreas {
  return resourceProxy(ws, path, SURFACE, OFFLINEAREAS_ACTIONS) as unknown as OfflineAreas;
}

