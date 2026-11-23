const seen = {};
const show = () => {
  if (seen.echo && seen.changed && seen.area && seen.closed) document.title = 'ported:' + JSON.stringify(seen);
};
chrome.runtime.onConnect.addListener((port) => {
  seen.named = port.name;
  port.onMessage.addListener((message, from) => {
    from.postMessage({pong: message.ping + ':' + (port.sender.id === chrome.runtime.id)});
  });
});
chrome.storage.onChanged.addListener((changes, area) => {
  if (area !== 'local' || !changes.color || changes.color.newValue !== 'blue') return;
  seen.changed = changes.color.oldValue + '>' + changes.color.newValue;
  show();
});
chrome.storage.local.onChanged.addListener((changes) => {
  if (changes.color && changes.color.newValue === 'blue') seen.area = true;
  show();
});
const port = chrome.runtime.connect({name: 'from-page'});
port.onMessage.addListener((message) => {
  Object.assign(seen, message);
  show();
});
port.onDisconnect.addListener(() => {
  seen.closed = true;
  show();
});
port.postMessage({hello: 'hello'});
