// The waits the gate uses to know the shell has done what the person asked,
// as state the page publishes — never a count of frames or a stopwatch.
//
// A page drawing a megapixel through a software rasteriser presents a few
// frames a second, and a shell with nothing left to draw presents none at all:
// so a frame count says what the machine is doing, not what the shell has
// finished. Every wait here ends when the shell's own answer says so, and the
// frames it may also look at are progress only while the shell still owes work.

import {availableParallelism} from 'node:os';
import {readFileSync, readdirSync} from 'node:fs';

/** The memory the gate's browsers hold, read from /proc while they run: each
 * renderer and the GPU process (which holds a software GPU's buffers), with
 * what each holds now and its peak, and what the machine has left. A renderer
 * that dies says what it held just before. */
export const browserMemory = () => {
  const kib = (text, field) => Number(text.match(new RegExp(`^${field}:\\s+(\\d+) kB`, 'm'))?.[1] ?? 0);
  const processes = [];
  for (const pid of readdirSync('/proc').filter(name => /^\d+$/.test(name))) {
    try {
      const command = readFileSync(`/proc/${pid}/cmdline`, 'utf8');
      const type = command.match(/--type=([a-z-]+)/)?.[1];
      if (!type || !['renderer', 'gpu-process'].includes(type)) continue;
      const status = readFileSync(`/proc/${pid}/status`, 'utf8');
      processes.push(`${type} ${pid}: ${Math.round(kib(status, 'VmRSS') / 1024)} MiB (peak ${Math.round(kib(status, 'VmHWM') / 1024)} MiB)`);
    } catch { /* gone between the listing and the read */ }
  }
  let available = '?';
  try { available = `${Math.round(kib(readFileSync('/proc/meminfo', 'utf8'), 'MemAvailable') / 1024)} MiB`; } catch {}
  return `${processes.join(', ') || 'no renderer or GPU process'}; the machine has ${available} available`;
};

/** How long one step of a page test may take before the run calls it hung
 * and says which step it was: one thing a person does and the page's answer.
 * TILO_STEP_SECONDS sets it (default 5), for a machine whose pages answer
 * slower; every wait in the drivers is a whole number of steps, so a hang
 * fails in seconds and a slow step is named by its own wait, never hidden
 * inside a minutes-long one. */
export const STEP_MS = Math.round(1000 * (Number(process.env.TILO_STEP_SECONDS) || 5));

/** The window a driven shell is drawn in. Every pixel of it goes through the
 * software rasteriser on every frame, so it is the smallest size that still
 * gets the shell's desktop layout (the narrow layouts begin below 720 px wide
 * and 600 px tall): 1024 by 640 draws a third fewer pixels than 1280 by 800.
 * `empty` is a point on the shell below the windows it opens, where a click
 * gives the page focus without pressing anything: 100 px above the bottom
 * edge, as it was in the larger window, since what the shell draws along that
 * edge keeps its size in pixels whatever the window's. */
export const SHELL = {width: 1024, height: 640, empty: {x: 512, y: 540}};

/** The device metrics that give a tab the shell's window. */
export const shellMetrics = {width: SHELL.width, height: SHELL.height, deviceScaleFactor: 1, mobile: false};

/** `n` steps, for what takes a person more than one: a page loading and
 * joining (6), an answer crossing to another person's page (4). */
export const stepLimit = n => n * STEP_MS;

/** The page's own word for whether the open search still has rows to find:
 * a source still to answer, or rows not yet cut to the card. A settled search
 * (and a closed one) reads false. */
export const searching = tab => tab.evaluate('globalThis.tiloMount?.state()?.searchSearching === true');

/** Whether the open search is up, as the page publishes it. */
export const searchOpen = tab => tab.evaluate('!!globalThis.tiloMount?.state()?.searchOpen');

/**
 * Wait until the shell has presented `by` more frames, or says it owes none: a
 * shell with nothing to draw presents none — it waits for what changes — so a
 * frame count is progress only while it still owes work. `owed` is the shell's
 * own answer to that, read each turn: the wait ends when the frames came or
 * `owed` says there is nothing left to draw, and never outlives the work it
 * waits for.
 *
 * `idleMs` ends the wait when the shell has presented no frame for that long,
 * for a shell that has no answer of its own to give: a shell drawing a
 * megapixel through a software rasteriser presents every second or two, so a
 * longer silence is a shell with nothing left to draw, and waiting for the
 * timeout instead spends it on every step.
 *
 * `evaluate` reads an expression in the page (`tab.evaluate` in the gate,
 * a stub in a test); `sleep` and `now` are the clock and pause, injectable
 * so a test can prove the wait ends on state without any frame advancing.
 * `onTimeout` runs when the frames did not come and the shell still owes them.
 */
export async function settle({
  evaluate,
  by = 2,
  owed = () => true,
  idleMs = 0,
  sleep = ms => new Promise(resolve => setTimeout(resolve, ms)),
  now = () => Date.now(),
  timeoutMs = stepLimit(3),
  pollMs = 100,
  onTimeout = null,
}) {
  const from = await evaluate('globalThis.tiloMount?.state()?.frames ?? 0');
  const deadline = now() + timeoutMs;
  let lastFrame = from;
  let lastChange = now();
  for (;;) {
    const frames = await evaluate('globalThis.tiloMount?.state()?.frames ?? 0');
    if (frames >= from + by) return 'frames';
    if (!await owed()) return 'settled';
    if (frames !== lastFrame) {
      lastFrame = frames;
      lastChange = now();
    } else if (idleMs && now() - lastChange >= idleMs) {
      return 'idle';
    }
    if (now() > deadline) {
      if (onTimeout) return onTimeout({from, by});
      return 'timeout';
    }
    await sleep(pollMs);
  }
}

/** [`settle`] over the open search's own answer: it ends when the search owes
 * no rows (`searchSearching` false) as much as when the frames came. */
export const settleSearch = (tab, {evaluate = tab.evaluate.bind(tab), ...rest} = {}) =>
  settle({evaluate, owed: () => searching(tab), ...rest});

/**
 * Reload `tab` and wait until the document that answers is the one it loaded,
 * with its shell drawn.
 *
 * `tab.reload()` resolves when the browser took the reload, not when the new
 * document replaced the old one. Until it does, the old document still answers
 * every read — its own shell, its own people — and a title read then is the
 * page that is going away: a reloaded tab read straight after its reload is
 * titled "", not after its workspace. So the document is marked before the
 * reload, and this ends once a document *without* the mark answers, its shell
 * says it is presented (`state().phase`), and it answers that way twice in a
 * row: which is when the page holds its workspace and titles itself after it.
 * The second read is what tells a page that stayed from one a navigation is
 * about to replace — every document gets a fresh `globalThis`, so the token
 * planted here is the document's identity, and two reads that disagree are two
 * documents.
 *
 * `tab` answers `evaluate` and `reload`; `sleep` and `now` are injectable so a
 * test proves the wait ends on the new document's own state.
 *
 * Resolves 'presented' when the new shell says it is up, 'stalled' when the
 * new document loaded but its shell never said so (a failure screen, say), and
 * 'timeout' when no new document answered at all: the caller says what the
 * page showed.
 */
export async function reloaded(
  tab,
  {
    evaluate = tab.evaluate.bind(tab),
    timeoutMs = stepLimit(8),
    pollMs = 100,
    sleep = ms => new Promise(resolve => setTimeout(resolve, ms)),
    now = () => Date.now(),
  } = {},
) {
  const mark = `__gateDocument${Math.random().toString(36).slice(2)}`;
  const token = '__gateDocumentSeen';
  await evaluate(`globalThis[${JSON.stringify(mark)}] = true`).catch(() => {});
  await tab.reload();
  const deadline = now() + timeoutMs;
  let presentedIn = null;
  for (;;) {
    const read = Math.random().toString(36).slice(2);
    const at = await evaluate(`JSON.stringify({
      old: globalThis[${JSON.stringify(mark)}] === true,
      ready: document.readyState === 'complete',
      phase: globalThis.tiloMount?.state?.()?.phase ?? null,
      seen: (globalThis[${JSON.stringify(token)}] ??= ${JSON.stringify(read)}),
    })`).then(text => JSON.parse(text ?? '{}'), () => ({old: true, ready: false, phase: null, seen: null}));
    const presented = !at.old && at.phase === 'presented';
    // The same document, its shell up, on two reads in a row.
    if (presented && at.seen === presentedIn) return 'presented';
    presentedIn = presented ? at.seen : null;
    if (now() > deadline) return at.old || !at.ready ? 'timeout' : 'stalled';
    await sleep(pollMs);
  }
}

/** What the machine was doing when a wait ran out, so a run that timed out
 * because the machine never gave the page a turn reads differently from one
 * where the page itself hung. It reads the load against this machine's cores,
 * the CPU pressure (the share of the last ten seconds in which something
 * runnable waited for a core) of the machine and of this job's own cgroup,
 * and how much of this job's CPU time its cgroup throttled.
 * `starved` is true when this job's own tasks waited for a core a quarter of
 * the last ten seconds or more (the machine's pressure where the job's own
 * cannot be read): then a frame or an answer that never came may be the
 * machine, not the product. The machine's load is shown but not judged: it
 * counts every job on the node, and a job can get its cores while the node's
 * load is several times its cores. */
export function machine() {
  const read = path => { try { return readFileSync(path, 'utf8'); } catch { return ''; } };
  const avg10 = text => Number(text.match(/^some avg10=([\d.]+)/m)?.[1] ?? NaN);
  const cores = availableParallelism();
  const [load1] = read('/proc/loadavg').split(' ').map(Number);
  const hostPressure = avg10(read('/proc/pressure/cpu'));
  const jobPressure = avg10(read('/sys/fs/cgroup/cpu.pressure'));
  const stat = Object.fromEntries(read('/sys/fs/cgroup/cpu.stat').trim().split('\n')
    .map(line => line.split(' ')).filter(pair => pair.length === 2).map(([k, v]) => [k, Number(v)]));
  const throttled = stat.throttled_usec ? `, throttled ${(stat.throttled_usec / 1e6).toFixed(1)} s in ${stat.nr_throttled} periods` : '';
  const pressure = Number.isFinite(jobPressure) ? jobPressure : hostPressure;
  const starved = pressure >= 25;
  const given = process.env.OMP_NUM_THREADS ? `, this job given ${process.env.OMP_NUM_THREADS} threads` : '';
  const text = `${starved ? 'STARVED' : 'not starved'}: load ${load1?.toFixed(1)} on ${cores} cores${given}, `
    + `cpu pressure ${Number.isFinite(hostPressure) ? hostPressure : '?'}% (machine) `
    + `${Number.isFinite(jobPressure) ? jobPressure : '?'}% (this job)${throttled}`;
  return {starved, text};
}
