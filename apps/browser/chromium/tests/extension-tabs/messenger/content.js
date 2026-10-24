chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message.ping) reply('pong:' + location.pathname + ':' + (sender.id === chrome.runtime.id));
});
chrome.runtime.onConnect.addListener((port) => {
  port.onMessage.addListener((message) => port.postMessage(message + ':' + port.name + ':' + (port.sender.id === chrome.runtime.id)));
});
chrome.runtime.sendMessage({hello: true}).then((answer) => {
  document.title = 'messenger:' + JSON.stringify(answer);
});
