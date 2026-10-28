// A click copies the page's address through an offscreen document, as
// extensions that copy links and passwords do, and tells the page how it
// went.
chrome.action.onClicked.addListener(async (tab) => {
  let outcome;
  try {
    if (!await chrome.offscreen.hasDocument()) {
      await chrome.offscreen.createDocument({url: 'offscreen.html', reasons: ['CLIPBOARD'], justification: 'Copy the address'});
    }
    outcome = await chrome.runtime.sendMessage({target: 'offscreen', text: 'copied: ' + tab.url});
  } catch (error) {
    outcome = {error: String(error && error.message || error)};
  }
  await chrome.scripting.executeScript({
    target: {tabId: tab.id}, func: (outcome) => { document.title = 'copier:' + JSON.stringify(outcome); }, args: [outcome]});
});
