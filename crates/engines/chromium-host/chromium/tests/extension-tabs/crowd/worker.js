// A worker many frames ask at once. It runs a script in each frame that
// said hello, in that frame alone and by the id its message came with
// (`sender.frameId`), and says what every one of them answered once all
// have: the page's address says how many frames it holds besides its own
// (`/crowd/<frames>`).
const ran = new Map();
chrome.runtime.onMessage.addListener((message, sender) => {
  if (!message.hello) return;
  chrome.scripting.executeScript({target: {tabId: sender.tab.id, frameIds: [sender.frameId]}, func: () => location.pathname})
    .then(([injected]) => injected.result, (error) => 'failed: ' + error.message)
    .then((result) => {
      ran.set(message.hello, result);
      const page = [...ran.keys()].find((path) => path.startsWith('/crowd/'));
      if (!page || ran.size < Number(page.split('/')[2]) + 1) return null;
      return chrome.scripting.executeScript({
        target: {tabId: sender.tab.id},
        func: (text) => { document.title = 'crowd:' + text; },
        args: [JSON.stringify(Object.fromEntries(ran))],
      });
    });
});
