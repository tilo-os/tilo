// A click injects a script into the page, as clipping and printing
// extensions do; it talks to the worker both ways and reads the storage.
chrome.runtime.onInstalled.addListener(() => chrome.storage.local.set({color: 'green'}));
chrome.action.onClicked.addListener(async (tab) => {
  await chrome.storage.local.set({color: 'green'});
  await chrome.scripting.executeScript({target: {tabId: tab.id}, files: ['inject.js']});
});
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (!message.injected) return;
  // Answer, then ask the injected script something back by the tab's id.
  reply({tab: sender.tab.id, url: sender.tab.url});
  setTimeout(async () => {
    const back = await chrome.tabs.sendMessage(sender.tab.id, {back: true});
    await chrome.scripting.executeScript({
      target: {tabId: sender.tab.id},
      func: (back) => { document.title += '|' + back; },
      args: [back],
    });
  }, 0);
});
