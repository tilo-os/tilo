// The waits the gate uses to know the desk has done what the person asked,
// as state the page publishes — never a count of frames or a stopwatch.
//
// A page drawing a megapixel through a software rasteriser presents a few
// frames a second, and a desk with nothing left to draw presents none at all:
// so a frame count says what the machine is doing, not what the desk has
// finished. Every wait here ends when the desk's own answer says so, and the
// frames it may also look at are progress only while the desk still owes work.

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
 * `evaluate` reads an expression in the page (`tab.evaluate` in the gate,
 * a stub in a test); `sleep` and `now` are the clock and pause, injectable
 * so a test can prove the wait ends on state without any frame advancing.
 * `onTimeout` runs when the frames did not come and the desk still owes them.
 */
export async function settle({
  evaluate,
  by = 2,
  owed = () => true,
  sleep = ms => new Promise(resolve => setTimeout(resolve, ms)),
  now = () => Date.now(),
  timeoutMs = 120_000,
  pollMs = 100,
  onTimeout = null,
}) {
  const from = await evaluate('globalThis.tiloMount?.state()?.frames ?? 0');
  const deadline = now() + timeoutMs;
  for (;;) {
    const frames = await evaluate('globalThis.tiloMount?.state()?.frames ?? 0');
    if (frames >= from + by) return 'frames';
    if (!await owed()) return 'settled';
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
