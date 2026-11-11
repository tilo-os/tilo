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
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

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

/** Maps — `maps.workspace`. */
export interface Workspace extends tilo.Resource {
  places: Place[];
  readonly recents: unknown;
  mode: TravelMode;
  transit_time: unknown;
  readonly route_plan: unknown;
  readonly transit_plan: unknown;
  acceptPlaces(args: {
    places: Place[];
  }): Promise<tilo.Run | void>;
  commitSearch(args: {
    query: string;
  }): Promise<tilo.Run | void>;
  clearRecents(): Promise<tilo.Run | void>;
  acceptRoute(args: {
    route: Route;
    origin_name: string;
    dest_name: string;
    mode: TravelMode;
    origin?: LatLon;
    dest?: LatLon;
  }): Promise<tilo.Run | void>;
  clearRoute(): Promise<tilo.Run | void>;
  acceptTransit(args: {
    transit: TransitJourney[];
    origin_name: string;
    dest_name: string;
    origin?: LatLon;
    dest?: LatLon;
  }): Promise<tilo.Run | void>;
  clearTransit(): Promise<tilo.Run | void>;
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
  deleteOfflineArea(args: {
    area: string;
  }): Promise<void>;
  clearOfflineAreas(): Promise<void>;
}

/** The `maps.workspace` kind at `path`. */
export function workspace(ws: tilo.Workspace, path: string): Workspace {
  return resourceProxy(ws, path) as unknown as Workspace;
}

/** Offline areas — `maps.offline-areas`. */
export interface OfflineAreas extends tilo.Resource {
  devices: Record<string, unknown>;
  record(args: {
    device: string;
    region: DownloadedRegion;
  }): Promise<tilo.Run | void>;
  forget(args: {
    device: string;
    area: string;
  }): Promise<tilo.Run | void>;
  clear(args: {
    device: string;
  }): Promise<tilo.Run | void>;
}

/** The `maps.offline-areas` kind at `path`. */
export function offlineAreas(ws: tilo.Workspace, path: string): OfflineAreas {
  return resourceProxy(ws, path) as unknown as OfflineAreas;
}

