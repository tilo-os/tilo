// The waits the gate uses to know the desk has done what the person asked,
// as state the page publishes — never a count of frames or a stopwatch.
//
// A page drawing a megapixel through a software rasteriser presents a few
// frames a second, and a desk with nothing left to draw presents none at all:
// so a frame count says what the machine is doing, not what the desk has
// finished. Every wait here ends when the desk's own answer says so, and the
// frames it may also look at are progress only while the desk still owes work.

/** How long one step of a page test may take before the run calls it hung
 * and says which step it was: one thing a person does and the page's answer.
 * TILO_STEP_SECONDS sets it (default 5), for a machine whose pages answer
 * slower; every wait in the drivers is a whole number of steps, so a hang
 * fails in seconds and a slow step is named by its own wait, never hidden
 * inside a minutes-long one. */
export const STEP_MS = Math.round(1000 * (Number(process.env.TILO_STEP_SECONDS) || 5));

/** The window a driven desk is drawn in. Every pixel of it goes through the
 * software rasteriser on every frame, so it is the smallest size that still
 * gets the desk's desktop layout (the narrow layouts begin below 720 px wide
 * and 600 px tall): 1024 by 640 draws a third fewer pixels than 1280 by 800.
 * `empty` is a point on the desk below the windows it opens, where a click
 * gives the page focus without pressing anything. */
export const DESK = {width: 1024, height: 640, empty: {x: 512, y: 560}};

/** The device metrics that give a tab the desk's window. */
export const deskMetrics = {width: DESK.width, height: DESK.height, deviceScaleFactor: 1, mobile: false};

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
 * Wait until the desk has presented `by` more frames, or says it owes none: a
 * desk with nothing to draw presents none — it waits for what changes — so a
 * frame count is progress only while it still owes work. `owed` is the desk's
 * own answer to that, read each turn: the wait ends when the frames came or
 * `owed` says there is nothing left to draw, and never outlives the work it
 * waits for.
 *
 * `idleMs` ends the wait when the desk has presented no frame for that long,
 * for a desk that has no answer of its own to give: a desk drawing a
 * megapixel through a software rasteriser presents every second or two, so a
 * longer silence is a desk with nothing left to draw, and waiting for the
 * timeout instead spends it on every step.
 *
 * `evaluate` reads an expression in the page (`tab.evaluate` in the gate,
 * a stub in a test); `sleep` and `now` are the clock and pause, injectable
 * so a test can prove the wait ends on state without any frame advancing.
 * `onTimeout` runs when the frames did not come and the desk still owes them.
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
