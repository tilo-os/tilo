// A keyboard shortcut as video speed controllers take one: each press once,
// a press seen again (by its key and time) not twice.
let presses = 0;
let last = '';
document.addEventListener('keydown', (event) => {
  const signature = `${event.code}_${event.key}_${event.timeStamp}_${event.type}`;
  if (signature === last || event.key !== 'd') return;
  last = signature;
  presses += 1;
  document.title = 'presses:' + presses;
}, true);
