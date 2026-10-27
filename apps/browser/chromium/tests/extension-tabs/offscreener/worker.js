// A worker that leaves DOM parsing to an offscreen document, as MV3
// extensions do: it makes the document, which it then finds, talks to and
// closes. The document asks the worker for the tabs, which it has no API for.
const reasons = {url: 'offscreen.html', reasons: ['DOM_PARSER'], justification: 'Parse the page'};

chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message.target !== 'worker') return;
  chrome.tabs.query({active: true, lastFocusedWindow: true}).then((tabs) => {
    reply({url: tabs[0] && tabs[0].url, from: new URL(sender.url).pathname, tabless: sender.tab === undefined});
  });
  return true;
});

chrome.action.onClicked.addListener(async (tab) => {
  const seen = {};
  try {
    seen.before = await chrome.offscreen.hasDocument();
    await chrome.offscreen.createDocument(reasons);
    seen.after = await chrome.offscreen.hasDocument();
    const contexts = await chrome.runtime.getContexts({contextTypes: ['OFFSCREEN_DOCUMENT']});
    seen.contexts = contexts.map((context) => new URL(context.documentUrl).pathname);
    seen.second = await chrome.offscreen.createDocument(reasons).then(() => 'made', (error) => error.message);
    const [{result: html}] = await chrome.scripting.executeScript({
      target: {tabId: tab.id}, func: () => document.documentElement.outerHTML});
    seen.parsed = await chrome.runtime.sendMessage({target: 'offscreen', html});
    // A script on the page reaches the document itself, as in Chrome.
    const [{result: direct}] = await chrome.scripting.executeScript({
      target: {tabId: tab.id},
      func: () => chrome.runtime.sendMessage({target: 'offscreen', html: '<title>from the page</title>'})});
    seen.direct = direct;
    await chrome.offscreen.closeDocument();
    seen.closed = await chrome.offscreen.hasDocument();
  } catch (error) {
    seen.error = String(error && error.message || error);
  }
  await chrome.scripting.executeScript({
    target: {tabId: tab.id}, func: (seen) => { document.title = 'offscreen:' + JSON.stringify(seen); }, args: [seen]});
});
