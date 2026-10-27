// The clipper's panel: a page of the extension in a frame of the page, as
// Evernote Web Clipper shows its own.
const panel = document.createElement('iframe');
panel.src = chrome.runtime.getURL('panel.html');
document.body.appendChild(panel);
