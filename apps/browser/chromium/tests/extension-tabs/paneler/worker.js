// A side panel beside the page: a click on the button opens it
// (openPanelOnActionClick), a keyboard shortcut opens it for the tab it was
// pressed on, and what the worker saw is kept for the test to read.
globalThis.seen = [];
chrome.sidePanel.setPanelBehavior({openPanelOnActionClick: true});
chrome.action.onClicked.addListener((tab) => seen.push('clicked:' + tab.id));
chrome.commands.onCommand.addListener((command, tab) => {
  // Within the listener, as Chrome asks: nothing awaited before it.
  chrome.sidePanel.open({tabId: tab.id}).then(() => seen.push('opened'), (error) => seen.push('refused:' + error.message));
});
// Outside any gesture, which Chrome refuses.
chrome.sidePanel.open({windowId: 1}).catch((error) => seen.push('refused:' + error.message));
