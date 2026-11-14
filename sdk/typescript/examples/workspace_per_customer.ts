/**
 * A workspace per customer, at a backend's real scale: hundreds at once.
 *
 * The backend holds one server key (`TILO_API_KEY`, reaching all its
 * workspaces) at an API server (`TILO_API_URL`). For each of its customers it
 *
 * 1. makes a workspace labelled with its own id for the customer
 *    (`customer_id`), all of them at once, and finds each again by that id;
 * 2. mints a key that reaches that workspace alone, and hands the key's MCP
 *    URL and token to the customer's agent;
 * 3. the agent connects over MCP with only that token: it lists, reads, runs
 *    an action and puts a file in its own workspace, and nothing it calls,
 *    over MCP or the REST API, reaches any other customer's workspace;
 * 4. revokes some customers' keys, which stop working at once, mid-session;
 * 5. exports each workspace, then deletes them all, which revokes every key
 *    still minted for them.
 *
 * It prints how long each step took (p50, p95, max) and, when `SAMPLE_PIDS`
 * names the host's processes (`relay:1234,api:5678`, each counted with its
 * children; `host:parent` names the process that started this one), their
 * memory and CPU through the run.
 *
 *     eval "$(os/containers/tilo-sh/stack.sh env)"   # TILO_API_URL
 *     eval "$(head -1 "$TILO_SH_STATE/operator.env")"  # TILO_API_KEY: the stack's operator's server key
 *     CUSTOMERS=300 bun sdk/typescript/examples/workspace_per_customer.ts
 *
 * Every step runs for every customer, whatever happened to the others: what
 * failed is counted with why, and the run exits non-zero if anything did.
 *
 * `CUSTOMERS` sets how many (300); `REVOKE_EVERY` revokes every nth
 * customer's key (3).
 */

import { readFileSync, readdirSync } from "node:fs";

import { Refused, Tilo, type MintedKey, type WorkspaceInfo } from "tilo-sdk";

const CUSTOMERS = Number(process.env.CUSTOMERS ?? "300");
const REVOKE_EVERY = Number(process.env.REVOKE_EVERY ?? "3");
/** Labels this run's workspaces apart from any others the key has. */
const RUN = `run-${Date.now().toString(36)}`;

function say(step: string, detail = ""): void {
  const at = new Date().toISOString().slice(11, 19);
  console.log(`[${at}] ${step}${detail ? `: ${detail}` : ""}`);
}

function check(condition: unknown, what: string): void {
  if (!condition) {
    throw new Error(what);
  }
}

function quantile(values: number[], q: number): number {
  const ordered = [...values].sort((a, b) => a - b);
  return ordered[Math.round((ordered.length - 1) * q)] ?? 0;
}

/** Each of `items` through `work` at once; how long each took, in ms. */
async function timed<T, R>(
  items: T[],
  work: (item: T, index: number) => Promise<R>,
): Promise<{ results: R[]; ms: number[]; wallMs: number }> {
  const began = performance.now();
  const ms: number[] = new Array(items.length).fill(0);
  const results = await Promise.all(
    items.map(async (item, index) => {
      const start = performance.now();
      const result = await work(item, index);
      ms[index] = performance.now() - start;
      return result;
    }),
  );
  return { results, ms, wallMs: performance.now() - began };
}

/** Why `work` failed, or `null` when it did not. */
async function attempt(work: () => Promise<unknown>): Promise<string | null> {
  try {
    await work();
    return null;
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
}

/** Every failure of a step, counted by why. */
const failed = new Map<string, Map<string, number>>();

/** Count the failures of `step` in `results` (`null` for each that worked), and say them. */
function failures(step: string, results: (string | null)[]): number {
  const why = new Map<string, number>();
  for (const failure of results) {
    if (failure !== null) {
      const reason = failure
        .replace(/cus_\S+?([:' ])/g, "<customer>$1")
        .replace(/[0-9a-f]{8}-[0-9a-f-]{27}/g, "<id>")
        .replace(/\d+ ms/g, "<n> ms")
        .slice(0, 300);
      why.set(reason, (why.get(reason) ?? 0) + 1);
    }
  }
  for (const [reason, count] of why) {
    say(`${step}: ${count} failed`, reason);
  }
  if (why.size > 0) {
    failed.set(step, why);
  }
  return results.filter((failure) => failure !== null).length;
}

function report(step: string, run: { ms: number[]; wallMs: number }): Record<string, number> {
  const row = {
    n: run.ms.length,
    wall_s: Number((run.wallMs / 1000).toFixed(2)),
    p50_ms: Math.round(quantile(run.ms, 0.5)),
    p95_ms: Math.round(quantile(run.ms, 0.95)),
    max_ms: Math.round(quantile(run.ms, 1)),
  };
  say(step, `${row.n} in ${row.wall_s} s, p50 ${row.p50_ms} ms, p95 ${row.p95_ms} ms, max ${row.max_ms} ms`);
  return row;
}

// -- the host's memory and CPU ----------------------------------------------

const TICKS_PER_SECOND = 100;
const PAGE_BYTES = 4096;

/** A process and every process under it. */
function tree(root: number): number[] {
  const children = new Map<number, number[]>();
  for (const entry of readdirSync("/proc")) {
    const pid = Number(entry);
    if (!Number.isInteger(pid)) {
      continue;
    }
    try {
      const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
      const parent = Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1]);
      children.set(parent, [...(children.get(parent) ?? []), pid]);
    } catch {
      // It ended while we looked.
    }
  }
  const found = [root];
  for (let at = 0; at < found.length; at += 1) {
    found.push(...(children.get(found[at] ?? -1) ?? []));
  }
  return found;
}

/** CPU ticks and resident bytes of a process with its children. */
function usage(root: number): { ticks: number; rss: number } {
  let ticks = 0;
  let rss = 0;
  for (const pid of tree(root)) {
    try {
      const fields = readFileSync(`/proc/${pid}/stat`, "utf8").split(") ")[1]?.split(" ") ?? [];
      ticks += Number(fields[11]) + Number(fields[12]);
      rss += Number(fields[21]) * PAGE_BYTES;
    } catch {
      // It ended while we looked.
    }
  }
  return { ticks, rss };
}

class Sampler {
  readonly processes: [string, number][];
  readonly peaks = new Map<string, { rss: number; cpu: number }>();
  readonly first = new Map<string, { ticks: number; rss: number; at: number }>();
  last = new Map<string, { ticks: number; at: number }>();
  timer: ReturnType<typeof setInterval> | null = null;

  constructor(spec: string) {
    this.processes = spec
      .split(",")
      .filter((part) => part.includes(":"))
      .map((part) => {
        const [name, pid] = part.split(":");
        // `parent`: the process that started this run (a host serving in process).
        return [name ?? "", pid === "parent" ? process.ppid : Number(pid)];
      });
  }

  start(): void {
    for (const [name, pid] of this.processes) {
      const now = usage(pid);
      this.first.set(name, { ...now, at: performance.now() });
      this.last.set(name, { ticks: now.ticks, at: performance.now() });
      this.peaks.set(name, { rss: now.rss, cpu: 0 });
    }
    this.timer = setInterval(() => this.sample(), 1000);
  }

  sample(): void {
    for (const [name, pid] of this.processes) {
      const now = usage(pid);
      const at = performance.now();
      const last = this.last.get(name) ?? { ticks: now.ticks, at };
      const cpu = ((now.ticks - last.ticks) / TICKS_PER_SECOND / ((at - last.at) / 1000)) * 100;
      const peak = this.peaks.get(name) ?? { rss: 0, cpu: 0 };
      this.peaks.set(name, { rss: Math.max(peak.rss, now.rss), cpu: Math.max(peak.cpu, cpu) });
      this.last.set(name, { ticks: now.ticks, at });
    }
  }

  stop(): Record<string, unknown> {
    if (this.timer !== null) {
      clearInterval(this.timer);
    }
    this.sample();
    const rows: Record<string, unknown> = {};
    for (const [name, pid] of this.processes) {
      const first = this.first.get(name);
      const end = usage(pid);
      const peak = this.peaks.get(name);
      if (first === undefined || peak === undefined) {
        continue;
      }
      const seconds = (performance.now() - first.at) / 1000;
      rows[name] = {
        rss_start_mb: Math.round(first.rss / 1e6),
        rss_peak_mb: Math.round(peak.rss / 1e6),
        rss_end_mb: Math.round(end.rss / 1e6),
        cpu_seconds: Number(((end.ticks - first.ticks) / TICKS_PER_SECOND).toFixed(1)),
        cpu_mean_pct: Math.round(((end.ticks - first.ticks) / TICKS_PER_SECOND / seconds) * 100),
        cpu_peak_pct: Math.round(peak.cpu),
      };
      say(`${name} (pid ${pid} and children)`, JSON.stringify(rows[name]));
    }
    return rows;
  }
}

// -- an agent's MCP client --------------------------------------------------

/** What an MCP call answered: its HTTP status and JSON-RPC body. */
interface McpReply {
  status: number;
  body: Record<string, any>;
}

/**
 * An agent's MCP session over streamable HTTP, as any MCP client opens one:
 * `initialize` at its URL with its bearer token, then calls under the
 * session id the server answered.
 */
class Agent {
  readonly url: string;
  readonly token: string;
  session: string | null = null;
  #id = 0;

  constructor(url: string, token: string) {
    this.url = url;
    this.token = token;
  }

  async post(message: Record<string, unknown>): Promise<McpReply> {
    const headers: Record<string, string> = {
      authorization: `Bearer ${this.token}`,
      accept: "application/json",
      "content-type": "application/json",
    };
    if (this.session !== null) {
      headers["mcp-session-id"] = this.session;
    }
    const reply = await fetch(this.url, { method: "POST", headers, body: JSON.stringify(message) });
    const session = reply.headers.get("mcp-session-id");
    if (session !== null) {
      this.session = session;
    }
    const text = await reply.text();
    let body: Record<string, any> = {};
    try {
      body = text === "" ? {} : JSON.parse(text);
    } catch {
      body = { text };
    }
    return { status: reply.status, body };
  }

  initialize(): Promise<McpReply> {
    this.session = null;
    return this.post({
      jsonrpc: "2.0",
      id: (this.#id += 1),
      method: "initialize",
      params: {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "customer-agent", version: "1" },
      },
    });
  }

  tools(): Promise<McpReply> {
    return this.post({ jsonrpc: "2.0", id: (this.#id += 1), method: "tools/list" });
  }

  /** A tool's typed answer, or why it was refused. */
  async call(name: string, args: Record<string, unknown>): Promise<McpReply> {
    return this.post({
      jsonrpc: "2.0",
      id: (this.#id += 1),
      method: "tools/call",
      params: { name, arguments: args, _meta: { "so.tilo.mcp": { structured: true } } },
    });
  }
}

/** Whether a tool call ran: a 200 with a result that is no error. */
function ran(reply: McpReply): boolean {
  return reply.status === 200 && reply.body.result !== undefined && reply.body.result.isError !== true;
}

/** Whether a call reached nothing: refused by HTTP, by JSON-RPC, or by the tool. */
function refused(reply: McpReply): boolean {
  return !ran(reply);
}

/** A field of a tool's answer: typed, or in the JSON its text carries. */
function field(reply: McpReply, name: string): unknown {
  const typed = reply.body.result?.structuredContent?.[name];
  if (typed !== undefined) {
    return typed;
  }
  const text = reply.body.result?.content?.[0]?.text ?? "";
  try {
    return JSON.parse(text)[name];
  } catch {
    return undefined;
  }
}

/** A tool's answer once it finished: a pending one is followed with `tilo_status`. */
async function finished(agent: Agent, reply: McpReply): Promise<McpReply> {
  const deadline = Date.now() + 120_000;
  let answer = reply;
  while (answer.body.result?.structuredContent?.status === "pending" && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 250));
    answer = await agent.call("tilo_status", {
      invocation_id: answer.body.result.structuredContent.invocation_id,
    });
  }
  return answer;
}

/** Put `bytes` into the agent's workspace over MCP: its `tilo.content:` reference. */
async function putFile(agent: Agent, bytes: Uint8Array): Promise<string> {
  const begun = await agent.call("tilo_put_begin", { size: bytes.length });
  check(ran(begun), `tilo_put_begin: ${JSON.stringify(begun.body)}`);
  const upload = field(begun, "upload");
  const appended = await agent.call("tilo_put_append", {
    upload,
    offset: 0,
    chunk: Buffer.from(bytes).toString("base64"),
  });
  check(ran(appended), `tilo_put_append: ${JSON.stringify(appended.body)}`);
  const finished = await agent.call("tilo_put_finish", { upload });
  check(ran(finished), `tilo_put_finish: ${JSON.stringify(finished.body)}`);
  return String(field(finished, "content") ?? "");
}

// -- one customer's agent ---------------------------------------------------

interface Customer {
  index: number;
  id: string;
  workspace: WorkspaceInfo;
  key: MintedKey;
  agent: Agent;
}

/** The customer's agent works in its own workspace, with only its own token. */
async function works(customer: Customer): Promise<void> {
  const { agent, workspace } = customer;
  const opened = await agent.initialize();
  check(opened.status === 200 && agent.session !== null, `${customer.id}: initialize: ${JSON.stringify(opened.body)}`);
  const listed = await agent.call("tilo_ls", {});
  check(ran(listed), `${customer.id}: tilo_ls: ${JSON.stringify(listed.body)}`);
  const page = await finished(
    agent,
    await agent.call("tilo_do", {
      path: "notes#create-page",
      args: { title: `Visa checklist for ${customer.id}`, markdown: "- passport\n- photos\n" },
    }),
  );
  check(
    ran(page) && page.body.result?.structuredContent?.status === "ok",
    `${customer.id}: notes#create-page: ${JSON.stringify(page.body).slice(0, 600)}`,
  );
  const read = await agent.call("tilo_ls", { path: "notes" });
  check(
    ran(read) && JSON.stringify(read.body).includes(customer.id),
    `${customer.id}: its page is in its notes: ${JSON.stringify(read.body).slice(0, 400)}`,
  );
  const content = await putFile(agent, new TextEncoder().encode(`passport of ${customer.id}\n`));
  check(content.startsWith("tilo.content:"), `${customer.id}: a file goes in: ${content}`);
  check(workspace.id !== "", "the workspace has an id");
}

/**
 * Nothing the customer's token calls reaches `other`'s workspace: not its MCP
 * URL, not a tool naming it, not a REST route under it, not the account.
 */
async function isolated(customer: Customer, other: Customer, url: string): Promise<void> {
  const what = `${customer.id}'s token, at ${other.id}'s workspace`;
  // Its MCP URL: the token does not reach that workspace.
  const there = new Agent(other.key.mcpUrl, customer.key.token);
  const opened = await there.initialize();
  check(opened.status === 403 && there.session === null, `${what}: MCP initialize answered ${opened.status}`);
  // The server's own MCP endpoint, naming the other workspace in each tool.
  const root = new Agent(`${url}/mcp`, customer.key.token);
  const rooted = await root.initialize();
  check(rooted.status === 200, `${what}: /mcp initialize: ${rooted.status}`);
  for (const [tool, args] of [
    ["tilo_ls", { workspace: other.workspace.id }],
    ["tilo_read", { workspace: other.workspace.id, path: "notes" }],
    ["tilo_do", { workspace: other.workspace.id, path: "notes#create-page", args: { title: "x", markdown: "x" } }],
    ["tilo_put_begin", { workspace: other.workspace.id, size: 4 }],
    ["tilo_do", { path: "account/workspaces#list" }],
    ["tilo_do", { path: "account/workspaces#delete", args: { workspace: other.workspace.id } }],
    ["tilo_do", { path: "account/keys#mint", args: { workspace: other.workspace.id } }],
  ] as [string, Record<string, unknown>][]) {
    const reply = await root.call(tool, args);
    check(refused(reply), `${what}: ${tool} ${JSON.stringify(args)} ran: ${JSON.stringify(reply.body).slice(0, 300)}`);
  }
  // In its own pinned session, a tool naming the other workspace.
  const own = await customer.agent.call("tilo_ls", { workspace: other.workspace.id });
  check(
    !JSON.stringify(own.body).includes(other.id),
    `${what}: its own session listed the other's: ${JSON.stringify(own.body).slice(0, 300)}`,
  );
  // The REST API, as the customer's token.
  const api = new Tilo({ url, key: customer.key.token, retryDeadlineMs: 0 });
  const visible = await api.workspaces.list();
  check(
    visible.workspaces.length === 1 && visible.workspaces[0]?.id === customer.workspace.id,
    `${what}: it lists ${visible.workspaces.map((ws) => ws.id).join(", ")}`,
  );
  const elsewhere = api.workspace(other.workspace.id);
  for (const [route, attempt] of [
    ["list", () => elsewhere.ls("notes")],
    ["read", () => elsewhere.read("notes")],
    ["action", () => elsewhere.do("notes#create-page", { title: "x", markdown: "x" })],
    ["upload", () => elsewhere.files.upload(new Blob(["x"]))],
    ["delete", () => api.workspaces.delete(other.workspace.id)],
    ["mint", () => api.keys.mint(other.workspace.id)],
  ] as [string, () => Promise<unknown>][]) {
    const answer = await attempt().then(
      () => "ran",
      (error) => (error instanceof Refused ? error.code : String(error)),
    );
    check(answer !== "ran", `${what}: REST ${route} ran`);
  }
}

// -- the run ----------------------------------------------------------------

async function main(): Promise<void> {
  const url = (process.env.TILO_API_URL ?? "").replace(/\/+$/, "");
  check(url !== "" && process.env.TILO_API_KEY, "TILO_API_URL and TILO_API_KEY name the API server and its server key");
  const backend = new Tilo();
  const sampler = new Sampler(process.env.SAMPLE_PIDS ?? "");
  sampler.start();
  const timings: Record<string, unknown> = {};
  const ids = Array.from({ length: CUSTOMERS }, (_, index) => `cus_${RUN}_${index}`);

  say("making", `${CUSTOMERS} workspaces at once, each labelled with its customer's id`);
  const made = await timed(ids, (id) =>
    backend.workspaces.create(`Applicant ${id}`, {
      labels: { customer_id: id, batch: RUN },
      idempotencyKey: `workspace-for-${id}`,
    }),
  );
  timings.create = report("made", made);
  check(new Set(made.results.map((ws) => ws.id)).size === CUSTOMERS, "every customer has its own workspace");
  check(made.results.every((ws, index) => ws.labels.customer_id === ids[index]), "each is labelled with its customer");

  const again = await backend.workspaces.create(`Applicant ${ids[0]}`, {
    labels: { customer_id: ids[0] ?? "", batch: RUN },
    idempotencyKey: `workspace-for-${ids[0]}`,
  });
  check(again.id === made.results[0]?.id, "creating a customer's workspace again answers the one made");

  const listing = await timed([0], () => backend.workspaces.list({ labels: { batch: RUN } }));
  timings.list = report("listed this run's", listing);
  check(listing.results[0]?.length === CUSTOMERS, `the batch lists ${CUSTOMERS}: ${listing.results[0]?.length}`);
  const found = await timed(ids, (id) => backend.workspaces.find({ customer_id: id }));
  timings.find = report("found each by its customer id", found);
  check(found.results.every((ws, index) => ws?.id === made.results[index]?.id), "each customer id finds its workspace");

  say("minting", "a key per workspace, reaching it alone");
  const minted = await timed(made.results, (ws, index) =>
    backend.keys.mint(ws.id, `Agent for ${ids[index]}`, 3600),
  );
  timings.mint = report("minted", minted);
  const customers: Customer[] = made.results.map((workspace, index) => {
    const key = minted.results[index] as MintedKey;
    check(key.mcpUrl.endsWith(`/w/${workspace.id}/mcp`), `the key's MCP URL is its workspace's: ${key.mcpUrl}`);
    return { index, id: ids[index] ?? "", workspace, key, agent: new Agent(key.mcpUrl, key.token) };
  });

  say("working", "every customer's agent at once, over MCP with its own token");
  // Every agent's run is kept, worked or not: what failed is reported with
  // why, and the rest of the run goes on with the agents that worked.
  const working = await timed(customers, (customer) => attempt(() => works(customer)));
  timings.agents_work = report("agents worked in their own workspaces", working);
  timings.agents_failed = failures("agents", working.results);
  const active = customers.filter((_, index) => working.results[index] === null);

  say("isolating", "each token against its neighbour's workspace, over MCP and REST");
  const isolation = await timed(active, (customer) =>
    attempt(() => isolated(customer, customers[(customer.index + 1) % customers.length] as Customer, url)),
  );
  timings.isolation = report("no token reached another workspace", isolation);
  timings.isolation_failed = failures("isolation", isolation.results);

  const revoking = active.filter((customer) => customer.index % REVOKE_EVERY === 0);
  say("revoking", `${revoking.length} customers' keys`);
  const revoked = await timed(revoking, (customer) => attempt(() => backend.keys.revoke(customer.key.grant)));
  timings.revoke = report("revoked", revoked);
  timings.revoke_failed = failures("revoke", revoked.results);
  const stopped = await timed(revoking, (customer) => attempt(async () => {
    const midSession = await customer.agent.call("tilo_ls", {});
    check(refused(midSession), `${customer.id}: its open session still answers after revocation`);
    const reopened = await customer.agent.initialize();
    check(
      reopened.status === 401 && reopened.body?.error?.code === "key_revoked",
      `${customer.id}: a revoked key opens a session: ${reopened.status} ${JSON.stringify(reopened.body)}`,
    );
    const rest = await new Tilo({ url, key: customer.key.token, retryDeadlineMs: 0 })
      .workspaces.list()
      .then(
        () => "ran",
        (error: { code?: unknown }) => String(error?.code ?? error),
      );
    check(rest === "key_revoked", `${customer.id}: a revoked key lists: ${rest}`);
  }));
  timings.revoked_stop = report("revoked keys stopped working", stopped);
  timings.revoked_stop_failed = failures("revoked keys stop", stopped.results);
  const kept = active.filter((customer) => customer.index % REVOKE_EVERY !== 0);
  const still = await timed(kept, (customer) => attempt(async () => {
    const listed = await customer.agent.call("tilo_ls", {});
    check(ran(listed), `${customer.id}: an unrevoked key stopped: ${JSON.stringify(listed.body).slice(0, 300)}`);
  }));
  timings.kept_work = report("the other keys still work", still);
  timings.kept_work_failed = failures("unrevoked keys", still.results);

  say("exporting", "each workspace before it goes");
  const exported = await timed(customers, (customer, index) => attempt(async () => {
    const bundle = await backend.workspace(customer.workspace.id).export();
    check(bundle.workspace.labels.customer_id === ids[index], `${customer.id}: its export carries its labels`);
    check(
      working.results[index] !== null || JSON.stringify(bundle.resources).includes(`Visa checklist for ${ids[index]}`),
      `${customer.id}: its export holds the page its agent wrote`,
    );
  }));
  timings.export = report("exported", exported);
  timings.export_failed = failures("export", exported.results);

  say("deleting", `all ${CUSTOMERS} at once`);
  const deleted = await timed(customers, (customer) => attempt(() => backend.workspaces.delete(customer.workspace.id)));
  timings.delete = report("deleted", deleted);
  timings.delete_failed = failures("delete", deleted.results);
  const left = await backend.workspaces.list({ labels: { batch: RUN } });
  timings.left = left.length;
  failures("left after deleting", left.workspaces.map((ws) => `${ws.id} is still there`));
  const gone = await timed(kept, (customer) => attempt(async () => {
    const reopened = await customer.agent.initialize();
    check(reopened.status === 401, `${customer.id}: its key outlived its workspace: ${reopened.status}`);
  }));
  timings.deleted_keys_stop = report("every key of a deleted workspace stopped", gone);
  timings.deleted_keys_stop_failed = failures("keys of deleted workspaces", gone.results);

  const host = sampler.stop();
  const why = Object.fromEntries([...failed].map(([step, reasons]) => [step, Object.fromEntries(reasons)]));
  console.log(JSON.stringify({ customers: CUSTOMERS, batch: RUN, timings, failures: why, host }, null, 2));
  backend.close();
  if (failed.size > 0) {
    throw new Error(`steps failed for some customers: ${[...failed.keys()].join(", ")} (above)`);
  }
}

await main();
