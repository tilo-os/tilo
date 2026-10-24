#pragma once
// Extensions signing in (`chrome.identity.launchWebAuthFlow`).
//
// Chrome opens the sign-in page an extension names in a window of its own and
// ends the call once that page goes to the extension's own address,
// `https://<id>.chromiumapp.org/`, with whatever the site appended (a code, a
// token). Every page here is an offscreen browser no window holds, so the
// Browser shows the page in a panel over the page in front instead, as it
// shows an extension's popup, and the host watches where the page goes
// (host.cc, OnBeforeBrowse). This keeps each sign-in from its start to its
// outcome:
//
// - The Browser shows the page in a tab it opens for the sign-in, and names
//   the sign-in to the host (Bind). A sign-in no tab took up in time ends:
//   the Browser could not show it (a private window runs no extensions).
// - The page going to the extension's address ends the sign-in with that
//   address, and its tab closes; the tab closing first ends it as refused,
//   with Chrome's words for each.
// - A sign-in shown to the person is shown once at a time for an extension:
//   a second one ends the first, as if the person had closed it.
// - A sign-in that may not ask the person (`interactive` false) is loaded
//   out of sight, as Chrome loads it: if its page loads without sending the
//   person back, or (`abortOnLoadForNonInteractive` false, for a site whose
//   script sends them back) takes too long, it ends as needing them.
// - The extension's script asks for the outcome (Wait) in turns, each held a
//   while: a worker a pending request alone would not keep awake stays awake
//   between them, and one that slept meanwhile asks no more.
#include <chrono>
#include <cstdint>
#include <functional>
#include <map>
#include <optional>
#include <string>
#include <utility>
#include <vector>

namespace tilo {

inline constexpr char kSignInRefused[] = "The user did not approve access.";
inline constexpr char kSignInNotShown[] = "Authorization page could not be loaded.";
inline constexpr char kSignInInteraction[] = "User interaction required.";

// The address a sign-in of `extension` ends at, and the start of every
// address under it.
inline std::string SignInRedirect(const std::string& extension) {
  return "https://" + extension + ".chromiumapp.org/";
}

// A page an extension may ask the person to sign in on: a web page.
inline bool SignInPage(const std::string& url) {
  const bool web = url.rfind("https://", 0) == 0 || url.rfind("http://", 0) == 0;
  return web && url.size() <= 8192 && url.find_first_of(" \t\r\n") == std::string::npos;
}

class ExtensionSignIn {
 public:
  using Clock = std::chrono::steady_clock;
  // How a sign-in ended: the address it went to, or why it did not.
  struct Outcome {
    std::string url;
    std::string error;
  };
  // Told the outcome, or nothing while the sign-in goes on.
  using Waiter = std::function<void(std::optional<Outcome>)>;
  // A tab the Browser must close: its sign-in ended without it.
  using Close = std::function<void(int tab)>;
  // The most sign-ins going on at once; the oldest ends for a new one.
  static constexpr size_t kMaxOpen = 16;

  explicit ExtensionSignIn(Close close) : close_(std::move(close)) {}

  // A sign-in of `extension` on `url` starts, shown to the person or not
  // (`interactive`), and if not, ending as its page loads or not
  // (`abort_on_load`): its number, by which the Browser names its tab and
  // the extension asks for the outcome.
  uint64_t Start(const std::string& extension, const std::string& url, bool interactive, bool abort_on_load,
                 Clock::time_point now) {
    Prune(now);
    auto earlier = Open([&](const Flow& flow) { return interactive && flow.interactive && flow.extension == extension; });
    const auto open = Open([](const Flow&) { return true; });
    if (open.size() >= kMaxOpen && (earlier.empty() || earlier.front() != open.front())) earlier.insert(earlier.begin(), open.front());
    for (const uint64_t number : earlier) EndAndClose(number, Outcome{{}, Refusal(flows_.at(number))}, now);
    const uint64_t number = ++last_;
    flows_[number] = Flow{extension, url, interactive, abort_on_load, 0, std::nullopt, nullptr, now};
    return number;
  }
  bool Interactive(uint64_t number) const {
    auto found = flows_.find(number);
    return found != flows_.end() && found->second.interactive;
  }

  // The Browser shows sign-in `number` of `extension` in `tab`.
  bool Bind(const std::string& extension, uint64_t number, int tab) {
    auto found = flows_.find(number);
    if (found == flows_.end() || found->second.extension != extension || found->second.tab || found->second.outcome ||
        tab <= 0 || Showing(tab))
      return false;
    found->second.tab = tab;
    return true;
  }
  // The page the sign-in `tab` shows is for, once it shows one.
  std::optional<std::string> PageOf(int tab) const {
    const auto* flow = Showing(tab);
    return flow ? std::optional<std::string>(flow->url) : std::nullopt;
  }

  // The sign-in tab `tab` is about to go to `url` (a link, a redirect, a
  // script): true when that is its extension's address, which ends the
  // sign-in. The page goes no further and its tab closes.
  bool Navigating(int tab, const std::string& url, Clock::time_point now) {
    for (const uint64_t number : Open([&](const Flow& flow) { return flow.tab == tab; })) {
      if (url.rfind(SignInRedirect(flows_.at(number).extension), 0) != 0) return false;
      EndAndClose(number, Outcome{url, {}}, now);
      return true;
    }
    return false;
  }

  // The sign-in tab `tab` finished loading `url`. A sign-in out of sight
  // whose page loaded without sending the person back needs them.
  void Loaded(int tab, const std::string& url, Clock::time_point now) {
    if (url.empty() || url == "about:blank") return;
    for (const uint64_t number : Open([&](const Flow& flow) { return flow.tab == tab && !flow.interactive && flow.abort_on_load; }))
      EndAndClose(number, Outcome{{}, kSignInInteraction}, now);
  }

  // The sign-in tab `tab` closed: the person turned the sign-in down, or
  // one out of sight could not go on without them.
  void Closed(int tab, Clock::time_point now) {
    for (const uint64_t number : Open([&](const Flow& flow) { return flow.tab == tab; }))
      End(number, Outcome{{}, Refusal(flows_.at(number))}, now);
  }

  // No tab took up sign-in `number`: the Browser could not show it.
  void Unshown(uint64_t number, Clock::time_point now) {
    auto found = flows_.find(number);
    if (found != flows_.end() && !found->second.tab && !found->second.outcome)
      End(number, Outcome{{}, kSignInNotShown}, now);
  }
  // Sign-in `number` out of sight took too long: it needs the person.
  void Expire(uint64_t number, Clock::time_point now) {
    auto found = flows_.find(number);
    if (found != flows_.end() && !found->second.interactive && !found->second.outcome)
      EndAndClose(number, Outcome{{}, kSignInInteraction}, now);
  }

  // The extension was removed or turned off: its sign-ins end, their tabs
  // close.
  void Forget(const std::string& extension) {
    for (auto it = flows_.begin(); it != flows_.end();) {
      if (it->second.extension != extension) { ++it; continue; }
      auto flow = std::move(it->second);
      it = flows_.erase(it);
      if (flow.waiter) flow.waiter(Outcome{{}, kSignInRefused});
      if (flow.tab && !flow.outcome && close_) close_(flow.tab);
    }
  }

  // The extension asks how sign-in `number` ended: told at once if it did,
  // else once it does or `Lapse` says it goes on. False for a sign-in that
  // is not the extension's, or no longer known.
  bool Wait(const std::string& extension, uint64_t number, Waiter waiter) {
    auto found = flows_.find(number);
    if (found == flows_.end() || found->second.extension != extension) return false;
    auto& flow = found->second;
    if (flow.outcome) {
      auto outcome = std::move(*flow.outcome);
      flows_.erase(found);
      waiter(std::move(outcome));
      return true;
    }
    // A worker woken anew asks again: the request it made before is gone.
    if (flow.waiter) std::exchange(flow.waiter, nullptr)(std::nullopt);
    flow.waiter = std::move(waiter);
    return true;
  }
  // The turn of the extension waiting on `number` is over: it is told the
  // sign-in goes on, and asks again.
  void Lapse(uint64_t number) {
    auto found = flows_.find(number);
    if (found != flows_.end() && found->second.waiter) std::exchange(found->second.waiter, nullptr)(std::nullopt);
  }

  size_t size() const { return flows_.size(); }

 private:
  struct Flow {
    std::string extension, url;
    bool interactive = true, abort_on_load = true;
    int tab = 0;
    std::optional<Outcome> outcome;
    Waiter waiter;
    Clock::time_point changed;
  };
  // An outcome no one asks for within this is dropped: the extension's
  // worker slept and its call went with it.
  static constexpr auto kKept = std::chrono::minutes(1);

  static const char* Refusal(const Flow& flow) { return flow.interactive ? kSignInRefused : kSignInInteraction; }
  const Flow* Showing(int tab) const {
    for (const auto& [number, flow] : flows_)
      if (flow.tab == tab && !flow.outcome) return &flow;
    return nullptr;
  }
  // The sign-ins going on that `which` picks, oldest first.
  template <typename Which>
  std::vector<uint64_t> Open(Which which) const {
    std::vector<uint64_t> numbers;
    for (const auto& [number, flow] : flows_)
      if (!flow.outcome && which(flow)) numbers.push_back(number);
    return numbers;
  }
  void EndAndClose(uint64_t number, Outcome outcome, Clock::time_point now) {
    const int tab = flows_.at(number).tab;
    End(number, std::move(outcome), now);
    if (tab && close_) close_(tab);
  }
  void End(uint64_t number, Outcome outcome, Clock::time_point now) {
    auto& flow = flows_.at(number);
    flow.changed = now;
    if (!flow.waiter) { flow.outcome = std::move(outcome); return; }
    auto waiter = std::move(flow.waiter);
    flows_.erase(number);
    waiter(std::move(outcome));
  }
  // Outcomes no one asked for in time go, and the oldest of them while
  // there are many.
  void Prune(Clock::time_point now) {
    for (auto it = flows_.begin(); it != flows_.end();)
      it = it->second.outcome && now - it->second.changed > kKept ? flows_.erase(it) : std::next(it);
    for (auto it = flows_.begin(); it != flows_.end() && flows_.size() >= 4 * kMaxOpen;)
      it = it->second.outcome ? flows_.erase(it) : std::next(it);
  }

  Close close_;
  std::map<uint64_t, Flow> flows_;
  uint64_t last_ = 0;
};

}  // namespace tilo
