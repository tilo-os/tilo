// Where a page test spends its time, said as each part of it begins: a run
// prints what each step took and what the whole took, so a slow run names the
// part that was slow without anyone timing it by hand, and two runs are
// compared by these lines.
//
//   const mark = clock();
//   … a step …
//   mark('Notes open');          TIMING step=start seconds=0.4 total=0.4
//   … the next step …
//   mark();                      TIMING step=Notes open seconds=2.1 total=2.5
//
// The line names the step that just ended, so a step that never ends (a wait
// that hangs) is the last name printed and the next one missing.
export function clock() {
  const began = Date.now();
  let since = began;
  let name = 'start';
  return next => {
    const now = Date.now();
    console.log(`TIMING step=${name} seconds=${((now - since) / 1000).toFixed(1)} total=${((now - began) / 1000).toFixed(1)}`);
    if (next !== undefined) {
      name = next;
      since = now;
    }
    return now - began;
  };
}
