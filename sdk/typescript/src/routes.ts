/**
 * Every route of `/v1` the SDK calls, and how a path goes into one.
 *
 * `docs/developers/api.md` is the table these follow, and
 * `crates/hosting/tilo-api/src/v1/operations.rs` is the one the API server
 * routes by: a route here that names no operation there is a 404.
 */

/** The API's major version, which this client speaks and every answer names. */
export const API_VERSION = "1";
/** Where the versioned API lives under the server's origin. */
const PREFIX = "/v1";

/**
 * A resource path as one URL segment: its own segments percent-encoded one by
 * one (`notes/Todo%20list`), so a name holding `/` is still one of them and is
 * written `notes/Q3%2F4%20plan`. A segment that is already encoded is read
 * first, so the same path may be written either way.
 */
export function encodePath(path: string): string {
  return path.split("/").map(segmentOf).join("/");
}

function segmentOf(part: string): string {
  try {
    return encodeURIComponent(decodeURIComponent(part));
  } catch {
    // A name holding a lone `%`: nothing to read first.
    return encodeURIComponent(part);
  }
}

/** A query, as `?a=b&c=d`: empty values left out, as the API reads them as absent. */
export function query(fields: Record<string, string | number | boolean | null | undefined>): string {
  const parts: string[] = [];
  for (const [name, value] of Object.entries(fields)) {
    if (value === null || value === undefined || value === "") {
      continue;
    }
    parts.push(`${encodeURIComponent(name)}=${encodeURIComponent(String(value))}`);
  }
  return parts.length === 0 ? "" : `?${parts.join("&")}`;
}

const segment = encodeURIComponent;

/** The API's routes, each a function of what names it. */
export const routes = {
  info: () => PREFIX,
  account: () => `${PREFIX}/account`,

  workspaces: () => `${PREFIX}/workspaces`,
  workspace: (workspace: string) => `${PREFIX}/workspaces/${segment(workspace)}`,

  keys: () => `${PREFIX}/keys`,
  key: (grant: string) => `${PREFIX}/keys/${segment(grant)}`,

  resources: (workspace: string) => `${PREFIX}/workspaces/${segment(workspace)}/resources`,
  resource: (workspace: string, path: string) =>
    `${PREFIX}/workspaces/${segment(workspace)}/resources/${encodePath(path)}`,
  action: (workspace: string, path: string, action: string) =>
    `${PREFIX}/workspaces/${segment(workspace)}/resources/${encodePath(path)}/actions/${segment(action)}`,

  runs: (workspace: string) => `${PREFIX}/workspaces/${segment(workspace)}/runs`,
  run: (workspace: string, run: string) =>
    `${PREFIX}/workspaces/${segment(workspace)}/runs/${segment(run)}`,
  runCancel: (workspace: string, run: string) =>
    `${PREFIX}/workspaces/${segment(workspace)}/runs/${segment(run)}/cancel`,
  runUndo: (workspace: string, run: string) =>
    `${PREFIX}/workspaces/${segment(workspace)}/runs/${segment(run)}/undo`,
  runOutput: (workspace: string, run: string) =>
    `${PREFIX}/workspaces/${segment(workspace)}/runs/${segment(run)}/output`,

  search: (workspace: string) => `${PREFIX}/workspaces/${segment(workspace)}/search`,
  links: (workspace: string) => `${PREFIX}/workspaces/${segment(workspace)}/links`,
  members: (workspace: string) => `${PREFIX}/workspaces/${segment(workspace)}/members`,
  invitations: (workspace: string) =>
    `${PREFIX}/workspaces/${segment(workspace)}/members/invitations`,
  programs: (workspace: string) => `${PREFIX}/workspaces/${segment(workspace)}/programs`,
  uploads: (workspace: string) => `${PREFIX}/workspaces/${segment(workspace)}/uploads`,
  content: (workspace: string, content: string) =>
    `${PREFIX}/workspaces/${segment(workspace)}/content/${segment(content)}`,
  events: (workspace: string) => `${PREFIX}/workspaces/${segment(workspace)}/events`,

  /** A new key, approved by the person: the device flow, beside the versioned API. */
  newKey: () => "/keys/new",
  pollKey: () => "/keys/poll",
};

/** `app/resource#action` split into the resource's path and the action's id. */
export function splitAction(path: string): { resource: string; action: string } {
  const at = path.lastIndexOf("#");
  if (at <= 0 || at === path.length - 1) {
    throw new Error(`\`${path}\` names no action: an action is \`app/resource#action\``);
  }
  return { resource: path.slice(0, at), action: path.slice(at + 1) };
}
