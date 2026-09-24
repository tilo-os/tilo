#pragma once

namespace qos {
// Remove renderer media contexts when their browser handle is destroyed.
// CEF may create a replacement before destroying the old handle with the same
// numeric ID. Preserve replacement frames, including cross-origin child frames.
// Browser handles are ref-counted and compared through CEF's IsSame contract.
template <class Contexts, class Browser>
void EraseMediaBrowserContexts(Contexts& contexts, const Browser& browser) {
  auto found = contexts.find(browser->GetIdentifier());
  if (found == contexts.end()) return;
  auto& frames = found->second;
  for (auto it = frames.begin(); it != frames.end();) {
    if (it->second.browser->IsSame(browser)) it = frames.erase(it);
    else ++it;
  }
  if (frames.empty()) contexts.erase(found);
}
}  // namespace qos
