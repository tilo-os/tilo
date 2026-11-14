// In every frame, as a password manager's: each asks its worker, as its
// document starts, for the script that finds the frame's fields.
chrome.runtime.sendMessage({hello: location.pathname});
