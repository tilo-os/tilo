(async () => {
  const answer = await chrome.runtime.sendMessage({ask: true});
  const port = chrome.runtime.connect({name: 'to-popup'});
  const back = await new Promise((resolve) => {
    port.onMessage.addListener(resolve);
    port.postMessage('hi');
  });
  document.title = 'paged:' + answer + '|' + back;
})().catch((error) => { document.title = 'failed:' + error.message; });
