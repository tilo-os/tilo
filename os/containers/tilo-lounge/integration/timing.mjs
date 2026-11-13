// Where a page test spends its time, said as each part of it ends: a run
// prints what each step took and what the whole took, so a slow run names the
// part that was slow without anyone timing it by hand, and two runs are
// compared by these lines.
//
//   const mark = clock();
//   … a step …
//   mark('Notes open');          TIMING step=Notes open seconds=2.1 total=2.5
//   … the last step …
//   mark();                      TIMING step=end seconds=0.6 total=3.1
//
// The line names the step that just ended, with what it took since the line
// before it, so a step that never ends (a wait that hangs) is the last name
// printed and the next one missing. A mark names the step it ends, which is
// what every driver here writes after the work: a line labelled with the step
// *before* the one that took the time sent whoever read it to the wrong step,
// and a hang-up was read as minutes of waiting when the minutes were the step
// after it.
export function clock() {
  const began = Date.now();
  let since = began;
  return next => {
    const now = Date.now();
    console.log(`TIMING step=${next ?? 'end'} seconds=${((now - since) / 1000).toFixed(1)} total=${((now - began) / 1000).toFixed(1)}`);
    since = now;
    return now - began;
  };
}
