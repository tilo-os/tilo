// A script injected on the tab's change, as AdGuard injects its keep-alive:
// it needs the content scripts' `chrome`, and says which world it ran in.
({connect: typeof chrome.runtime.connect, declared: globalThis.declared === true});
