// A page of the extension in a frame of a web page, as download managers
// show their own. It asks the host's bridge itself, as extension_tabs.js
// does where it runs, to open a download when its worker says, and tells
// how that went.
const ask = (method, args) => fetch('https://extension-bridge.tilo.invalid/' + method, {
  method: 'POST',
  headers: {'Content-Type': 'text/plain'},
  body: JSON.stringify({token: null, args}),
  credentials: 'omit',
  cache: 'no-store',
}).then((response) => response.json());
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message.ping) { reply('ready'); return false; }
  ask('downloads.open', {downloadId: message.open})
    .then((answer) => reply(answer.error || 'opened'), (error) => reply(error.message));
  return true;
});
