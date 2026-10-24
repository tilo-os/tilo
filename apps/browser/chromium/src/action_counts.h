#pragma once
// What extensions' network rules (declarativeNetRequest) did to the requests
// of each of the host's tabs, counted for the page as Chrome counts it for an
// extension's button (`displayActionCountAsBadgeText`).
//
// Chromium counts what a rule did in one of the Browser's tabs for no tab:
// its rules find no tab id there (-1), so it shows no count and answers for
// none. The Browser's own extension sees each request a rule blocked, or
// redirected to an extension's own file, by the tab webRequest reports it
// for; the host keeps them here for the page the tab shows, and counts each
// for the extensions whose rules acted on it.
#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <map>
#include <optional>
#include <set>
#include <string>
#include <utility>
#include <vector>

namespace tilo {

// A request a rule acted on in a tab.
struct RuleAction {
  std::string url, type, method, initiator;
  // The extension whose own file a redirect went to; empty for a block,
  // which names no extension.
  std::string owner;
  // When it was acted on, in milliseconds since the epoch, as webRequest
  // stamps it.
  double time = 0;
};

class ActionCounts {
 public:
  // The most requests kept for one page; counts go on past it.
  static constexpr size_t kMaxActions = 1000;
  // A page's first requests may be acted on before the host hears the page
  // committed: those of the last second before are the new page's.
  static constexpr double kCommitSlack = 1000;

  // The tab began loading at `now`: a page it commits next began then.
  void Loading(int tab, double now) { tabs_[tab].loading = now; }

  // The tab committed a new page at `now`. What was acted on since it began
  // loading is the new page's; the old page's counts go with it, as do the
  // extensions' own changes to them.
  void Navigated(int tab, double now) {
    auto& state = tabs_[tab];
    state.start = std::max(state.loading, now - kCommitSlack);
    std::erase_if(state.actions, [&](const Entry& entry) { return entry.action.time < state.start; });
    state.counts.clear();
    for (const auto& entry : state.actions)
      for (const auto& extension : entry.counted) ++state.counts[extension];
  }

  void Removed(int tab) { tabs_.erase(tab); }

  // The extension is gone: nothing is counted for it.
  void Forget(const std::string& extension) {
    for (auto& [tab, state] : tabs_) {
      state.counts.erase(extension);
      for (auto& entry : state.actions) entry.counted.erase(extension);
    }
  }

  // Keep a request acted on in the tab: the key to count it by, or none for
  // one acted on before the page the tab shows began.
  std::optional<uint64_t> Add(int tab, RuleAction action) {
    auto& state = tabs_[tab];
    if (action.time < state.start) return std::nullopt;
    if (state.actions.size() >= kMaxActions) state.actions.erase(state.actions.begin());
    state.actions.push_back(Entry{++sequence_, std::move(action), {}});
    return sequence_;
  }

  // Count the request `key` of the tab for the extension whose rules acted
  // on it, once: false when it was counted already, or is no longer the
  // page's.
  bool Count(int tab, uint64_t key, const std::string& extension) {
    auto found = tabs_.find(tab);
    if (found == tabs_.end()) return false;
    auto& actions = found->second.actions;
    auto entry = std::find_if(actions.begin(), actions.end(), [&](const Entry& each) { return each.key == key; });
    if (entry == actions.end() || !entry->counted.insert(extension).second) return false;
    ++found->second.counts[extension];
    return true;
  }

  // The extension changes its count for the page by `by`; it stays at zero
  // or above.
  void Increment(int tab, const std::string& extension, int by) {
    auto& count = tabs_[tab].counts[extension];
    count = std::max(0, count + by);
  }

  int Of(int tab, const std::string& extension) const {
    auto found = tabs_.find(tab);
    if (found == tabs_.end()) return 0;
    auto count = found->second.counts.find(extension);
    return count == found->second.counts.end() ? 0 : count->second;
  }

  // The requests acted on in the page the tab shows, oldest first.
  std::vector<RuleAction> Actions(int tab) const {
    std::vector<RuleAction> actions;
    if (auto found = tabs_.find(tab); found != tabs_.end())
      for (const auto& entry : found->second.actions) actions.push_back(entry.action);
    return actions;
  }

  // The same, each with the key to count it by: what an extension that
  // starts showing its count counts of the page so far.
  std::vector<std::pair<uint64_t, RuleAction>> Keyed(int tab) const {
    std::vector<std::pair<uint64_t, RuleAction>> keyed;
    if (auto found = tabs_.find(tab); found != tabs_.end())
      for (const auto& entry : found->second.actions) keyed.emplace_back(entry.key, entry.action);
    return keyed;
  }

 private:
  struct Entry {
    uint64_t key;
    RuleAction action;
    std::set<std::string> counted;
  };
  struct Tab {
    double loading = 0, start = 0;
    std::vector<Entry> actions;
    std::map<std::string, int> counts;
  };
  std::map<int, Tab> tabs_;
  uint64_t sequence_ = 0;
};

}  // namespace tilo
