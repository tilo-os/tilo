//! The two strings the engine's own extension tests read from the extension manager they
//! load: what Chromium answers while an extension's button shows a page's rule count, and the
//! expression that reads an action's badge, title, popup and switch from its service worker.
//!
//! The manager extension lives beside this file (`assets/extension-manager`), and its page is
//! driven by the engine's extension tests (`chromium/tests/extension_tabs.py`), which read
//! these constants as text. What a toolbar draws with them is the app's, not the engine's.

/// What Chromium answers for the badge of every tab while an extension's button shows each
/// page's count of what its rules did there: no text of its own.
#[allow(dead_code)]
const RULE_COUNT_PLACEHOLDER: &str = "<<declarativeNetRequestActionCount>>";

/// Reads the badge, title, popup and whether the button is on from `chrome.action`, as it is
/// for every tab.
#[allow(dead_code)]
const READ_ACTION: &str = "(async()=>{const a=chrome.action||chrome.browserAction;\
const [text,title,popup,enabled]=await Promise.all([a.getBadgeText({}),a.getTitle({}),\
a.getPopup?a.getPopup({}):null,a.isEnabled?a.isEnabled():true]);\
return JSON.stringify({text,title,popup,enabled})})()";
