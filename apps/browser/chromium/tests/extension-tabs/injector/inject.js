chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message.back) reply('back:' + (sender.id === chrome.runtime.id));
});
(async () => {
  const {color} = await chrome.storage.local.get('color');
  const answer = await chrome.runtime.sendMessage({injected: true});
  document.title = 'injected:' + JSON.stringify({
    color,
    known: typeof answer.tab === 'number' && answer.url === location.href,
    greeting: chrome.i18n.getMessage('greeting', ['tilo']),
    url: chrome.runtime.getURL('x.html') === 'chrome-extension://' + chrome.runtime.id + '/x.html',
  });
})().catch((error) => { document.title = 'failed:' + error.message; });
