// A Discord voice channel and a workspace voice channel carried as one call.
//
// This program is the call's outside half: Discord's voice gateway speaks to it, and what it
// hears is handed to the workspace as the person who said it, by their own name there. What the
// workspace's people say comes back as one Opus stream, the speaker the workspace's call lets
// through at a time, and is sent untouched: nothing is decoded, mixed or re-encoded here.
//
// The workspace half is one lane, opened by the daemon for this app's own connection
// (`TILO_SVC_NATIVE_FD`, the SDK's `callLane`): `carry` says which call is bridged, `report` says
// who is in the outside call and where this side stands, `heard` hands over each outside frame,
// `said` takes the workspace's frames and `asked` says where the daemon wants the bot: in the
// Discord call while anyone is on either side, showing who is on the workspace's side as the
// channel's status, and out of it once nobody is. The daemon holds the call's rule and its media
// keys, so this app never sees either, and a lane it is not granted is refused there.

import { SELF, callPeople, voicePairs } from "./plan.js";
import { keepAvatar } from "./avatars.js";

/// How often the workspace's frames are taken: one Opus frame.
const PUMP_MS = 20;

/// The account record's own path, where the bridge's state is.
export const ACCOUNTS = `${SELF}/accounts`;

/// The call lanes of the pairs the bridge carries, from what the workspace told it.
///
/// `calls`: the workspace client (its `read` and the lane below). `lane`: the workspace's call
/// lane for this app's connection. `gateway`: the Discord gateway this process holds.
export function startCalls({
  calls,
  lane,
  gateway,
  makeVoice,
  fetch = globalThis.fetch,
  content = (bytes) => calls.content?.(bytes) ?? null,
  showStatus = () => Promise.resolve(),
  log = () => {},
  onState = () => {},
}) {
  return new Carried({ calls, lane, gateway, makeVoice, fetch, content, showStatus, log, onState });
}

export class Carried {
  constructor({ calls, lane, gateway, makeVoice, fetch, content, showStatus, log, onState }) {
    this.calls = calls;
    /// Show a line in a Discord voice channel's status: `(channel, text)`, empty clears it.
    this.showStatus = showStatus;
    this.lane = lane;
    this.gateway = gateway;
    this.makeVoice = makeVoice;
    this.fetch = fetch;
    this.content = content;
    this.log = log;
    this.onState = onState;
    /// Where each avatar address was kept in the workspace's content, by the address.
    this.avatars = new Map();
    this.carried = new Map();
    this.states = new Map();
    this.pump = null;
    this.closed = false;
    this.ticked = false;
  }

  /// Read the bridge's record and carry every paired call it names.
  async start() {
    const accounts = await this.calls.state(ACCOUNTS);
    const account = accounts?.accounts?.[0] ?? accounts;
    if (!account?.guild_id) {
      this.log("no Discord server is connected");
      return;
    }
    this.account = account;
    this.pairs = voicePairs(account);
    if (this.pairs.length === 0) {
      this.log("no voice channel is paired with a call here");
      return;
    }
    this.pump = setInterval(() => this.#take(), PUMP_MS);
    this.timer = setInterval(() => this.#everySecond(), 1_000);
    for (const pair of this.pairs) await this.#carry(pair);
  }

  /// Read the bridge's record again, now that it changed while the gateway holds: a channel
  /// paired with a call since is carried at once, and one no longer paired lets its call go.
  async refresh(account) {
    if (this.closed || !account?.guild_id) return;
    this.account = account;
    const pairs = voicePairs(account);
    const wanted = new Set(pairs.map((pair) => pair.discord));
    for (const discord of [...this.carried.keys()]) {
      if (wanted.has(discord)) continue;
      await this.#ended(discord, "no longer paired with a call here");
      this.carried.delete(discord);
    }
    this.pairs = pairs;
    if (pairs.length === 0) return;
    if (!this.timer) this.timer = setInterval(() => this.#everySecond(), 1_000);
    if (!this.pump) this.pump = setInterval(() => this.#take(), PUMP_MS);
    for (const pair of pairs) {
      if (!this.carried.has(pair.discord)) await this.#carry(pair);
    }
  }

  /// What the gateway says about a call: someone joined, left, muted or went deaf.
  hear(event) {
    if (event?.state) {
      const state = event.state;
      if (state.user && state.user === (this.gateway.user?.id ?? "")) {
        // The bot left the call: Discord moved it out, or the person ended it there. A call the
        // bridge left itself, because nobody was on either side, is still carried.
        if (!state.channel) {
          for (const carried of this.carried.values()) {
            if (carried.guild === state.guild && carried.opened && !carried.leaving) {
              this.#ended(carried.pair.discord, "Discord ended the call");
            }
          }
        }
        return;
      }
      this.states.set(`${state.guild}/${state.user}`, state);
      this.#reportState(state.guild, state.channel);
      return;
    }
    const server = event;
    if (!server?.guild) return;
    const carried = this.carried.get(server.guild);
    if (!carried || carried.opened || !carried.wanted) return;
    if (!server.channel || !server.endpoint || !server.token) return;
    this.#open(carried, server);
  }

  /// Say who is in each call again: Discord's voice states reach the workspace only through
  /// here, and a mutation or a rejoin the gateway did not narrate still shows.
  #everySecond() {
    if (this.closed) return;
    for (const carried of this.carried.values()) {
      if (!carried.ended) this.#reportState(carried.guild, carried.pair.discord);
    }
  }

  /// Carry `pair` into the workspace's call, and say who is in its Discord call: the daemon asks
  /// the bot in once anyone is on either side.
  async #carry(pair) {
    const guild = this.account.guild_id;
    const voice = { guild, pair, opened: false, voice: null, problem: "", wanted: false, shown: "" };
    this.carried.set(pair.discord, voice);
    try {
      await this.lane.carry({ outside: pair.discord, tilo: pair.tilo });
    } catch (error) {
      voice.problem = `${error?.message ?? error}`;
      voice.refused = true;
      await this.#reportProblem(voice.problem);
    }
    await this.#reportState(guild, pair.discord);
  }

  /// Where the daemon wants the bot: in the Discord call `asked.call`, showing `asked.status`
  /// there, or in none.
  async #asked(asked) {
    for (const carried of this.carried.values()) {
      if (carried.ended || carried.refused) continue;
      if (asked.call === carried.pair.discord) {
        await this.#join(carried, asked.status ?? "");
      } else if (carried.wanted) {
        await this.#leave(carried);
      }
    }
  }

  /// Move the bot into `carried`'s Discord call, or keep it there, showing `status`.
  async #join(carried, status) {
    if (carried.shown !== status) {
      carried.shown = status;
      this.showStatus(carried.pair.discord, status).catch((error) =>
        this.log(`the voice channel's status was not set: ${error?.message ?? error}`),
      );
    }
    if (carried.wanted) return;
    carried.wanted = true;
    carried.leaving = false;
    carried.failed = false;
    carried.problem = "";
    await this.#reportState(carried.guild, carried.pair.discord);
    try {
      const server = await this.gateway.joinVoice(carried.guild, carried.pair.discord);
      if (carried.wanted) await this.#open(carried, server);
    } catch (error) {
      if (!carried.wanted) return;
      carried.failed = true;
      carried.problem = `${error?.message ?? error}`;
      await this.#reportProblem(carried.problem);
      await this.#reportState(carried.guild, carried.pair.discord);
    }
  }

  /// Move the bot out of `carried`'s Discord call: nobody is on either side. The pair stays
  /// carried, so the next person on either side brings it back.
  async #leave(carried) {
    carried.wanted = false;
    carried.leaving = true;
    carried.voice?.close();
    carried.voice = null;
    carried.opened = false;
    carried.failed = false;
    carried.problem = "";
    try {
      this.gateway.leaveVoice(carried.guild);
    } catch {}
    if (carried.shown) {
      carried.shown = "";
      this.showStatus(carried.pair.discord, "").catch(() => {});
    }
    await this.#reportState(carried.guild, carried.pair.discord);
  }

  async #open(carried, server) {
    if (carried.voice || carried.failed) return;
    carried.joining = true;
    try {
      const voice = await this.makeVoice({
        endpoint: server.endpoint,
        guild: carried.guild,
        channel: carried.pair.discord,
        user: this.gateway.user?.id ?? "",
        session: server.session,
        token: server.token,
        onAudio: (frame) => {
          // What this call heard never goes back to it: only outside frames reach here.
          this.lane.heard(frame).catch((error) =>
            this.log(`an outside frame did not reach the workspace: ${error?.message ?? error}`),
          );
        },
        onEnd: (reason) => {
          this.#ended(carried.pair.discord, reason);
        },
        log: this.log,
      });
      carried.voice = voice;
      carried.opened = true;
      carried.problem = "";
      await this.#reportProblem("");
    } catch (error) {
      carried.failed = true;
      carried.problem = `${error?.message ?? error}`;
      await this.#reportProblem(carried.problem);
      await this.#reportState(carried.guild, carried.pair.discord);
    } finally {
      carried.joining = false;
    }
  }

  /// One outside frame's worth: hand over what the workspace said to Discord, and go where the
  /// daemon asks the bot to be.
  async #take() {
    if (this.closed || this.taking) return;
    this.taking = true;
    try {
      const asked = await this.lane.asked?.();
      if (asked) await this.#asked(asked);
    } catch (error) {
      this.log(`the workspace's call did not say where the bot is wanted: ${error?.message ?? error}`);
    }
    try {
      const said = await this.lane.said();
      for (const frame of said ?? []) {
        for (const carried of this.carried.values()) {
          if (!carried.voice || carried.voice.muted) continue;
          carried.voice.say(frame.opus);
        }
      }
    } catch (error) {
      this.log(`the workspace's voice did not reach Discord: ${error?.message ?? error}`);
    } finally {
      this.taking = false;
    }
  }

  /// What the outside call holds now, as the workspace's call shows it.
  async #reportState(guild, channel) {
    const carried = [...this.carried.values()].find(
      (held) => held.guild === guild && held.pair.discord === channel,
    );
    if (!carried) return;
    const people = callPeople([...this.states.values()], {
      guild,
      channel,
      self: this.gateway.user?.id ?? "",
      avatars: this.avatars,
    });
    await Promise.all(
      people.map(async (person) => {
        if (!person.avatar && person.avatar_url) {
          const hash = await this.#avatar(person.avatar_url);
          if (hash) person.avatar = hash;
        }
      }),
    );
    const link = carried.opened
      ? { link: "joined", call: channel }
      : carried.problem
        ? { link: "refused", call: channel, reason: carried.problem }
        : carried.wanted
          ? { link: "joining", call: channel }
          : { link: "idle" };
    try {
      await this.lane.report({ calls: [{ call: channel, people }], link });
    } catch (error) {
      this.log(`the call's people did not reach the workspace: ${error?.message ?? error}`);
    }
  }

  /// The workspace's content hash of the picture at `url`, fetched once per address.
  async #avatar(url) {
    if (this.avatars.has(url)) return this.avatars.get(url);
    this.avatars.set(url, "");
    const hash = await keepAvatar(url, { fetch: this.fetch, content: this.content });
    if (!hash) this.log(`the avatar at ${url} was not kept`);
    this.avatars.set(url, hash ?? "");
    return hash ?? "";
  }

  async #reportProblem(problem) {
    try {
      await this.calls.do({
        app: SELF,
        action: "report_voice",
        args: { problem },
      });
    } catch (error) {
      this.log(`the voice problem was not recorded: ${error?.message ?? error}`);
    }
  }

  async #ended(channel, reason) {
    const carried = this.carried.get(channel);
    if (!carried || carried.ended) return;
    carried.ended = true;
    carried.wanted = false;
    carried.voice?.close();
    carried.voice = null;
    carried.opened = false;
    carried.problem = reason;
    try {
      this.gateway.leaveVoice(carried.guild);
    } catch {}
    try {
      await this.lane.carry(null);
    } catch {}
    await this.#reportProblem(reason);
    this.onState({ channel, reason });
  }

  close() {
    this.closed = true;
    if (this.pump) clearInterval(this.pump);
    if (this.timer) clearInterval(this.timer);
    for (const carried of this.carried.values()) carried.voice?.close();
    this.carried.clear();
  }
}
