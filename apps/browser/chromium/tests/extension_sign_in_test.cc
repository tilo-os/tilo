#include "extension_sign_in.h"
#include <cassert>
#include <optional>
#include <string>
#include <vector>

using tilo::ExtensionSignIn;

namespace {

const std::string kOne = "abcdefghijklmnopabcdefghijklmnop";
const std::string kTwo = "bbcdefghijklmnopabcdefghijklmnop";
const std::string kPage = "https://accounts.example/authorize";

// What a waiter was told: the address, the error, or "going on".
struct Told {
  std::vector<std::string> said;
  ExtensionSignIn::Waiter waiter() {
    return [this](std::optional<ExtensionSignIn::Outcome> outcome) {
      said.push_back(!outcome ? "going on" : outcome->error.empty() ? outcome->url : "error: " + outcome->error);
    };
  }
};

std::string Error(const char* message) { return std::string("error: ") + message; }

}  // namespace

int main() {
  auto now = ExtensionSignIn::Clock::now();
  std::vector<int> closed;
  ExtensionSignIn flows([&](int tab) { closed.push_back(tab); });

  // Only a web page is one to sign in on.
  assert(tilo::SignInPage("https://accounts.example/authorize?client_id=1"));
  assert(tilo::SignInPage("http://127.0.0.1:8080/authorize"));
  for (const char* refused : {"chrome-extension://abc/page.html", "javascript:alert(1)", "file:///etc/passwd", "",
                              "https://example.test/a b"})
    assert(!tilo::SignInPage(refused));
  assert(!tilo::SignInPage("https://example.test/" + std::string(8192, 'a')));
  assert(tilo::SignInRedirect(kOne) == "https://" + kOne + ".chromiumapp.org/");

  // The page goes to the extension's own address: the sign-in ends there,
  // the navigation is turned away and the panel closes. What the extension
  // waits on hears of it.
  const auto first = flows.Start(kOne, kPage, true, true, now);
  assert(flows.Interactive(first));
  // Only the sign-in's own extension and number take up a tab.
  assert(!flows.Bind(kTwo, first, 2));
  assert(!flows.Bind(kOne, first + 1, 2));
  assert(flows.Bind(kOne, first, 2));
  assert(!flows.Bind(kOne, first, 3));
  assert(flows.PageOf(2) == kPage);
  Told told;
  assert(flows.Wait(kOne, first, told.waiter()));
  assert(told.said.empty());
  // Its turn lapses: it is told to ask again.
  flows.Lapse(first);
  assert(told.said == std::vector<std::string>{"going on"});
  assert(flows.Wait(kOne, first, told.waiter()));
  // The sign-in page's own navigations, and other addresses, go ahead; an
  // interactive page that loaded goes on.
  assert(!flows.Navigating(2, "https://accounts.example/consent", now));
  assert(!flows.Navigating(2, "https://" + kTwo + ".chromiumapp.org/cb?code=1", now));
  assert(!flows.Navigating(2, "https://" + kOne + ".chromiumapp.org.example/cb", now));
  assert(!flows.Navigating(3, "https://" + kOne + ".chromiumapp.org/cb", now));
  flows.Loaded(2, "https://accounts.example/consent", now);
  assert(closed.empty());
  assert(flows.Navigating(2, "https://" + kOne + ".chromiumapp.org/cb?code=xyz", now));
  assert(told.said.back() == "https://" + kOne + ".chromiumapp.org/cb?code=xyz");
  assert(closed == std::vector<int>{2});
  assert(flows.size() == 0);
  // The panel's closing afterwards changes nothing.
  flows.Closed(2, now);
  assert(told.said.size() == 2);

  // The outcome waits for an extension that asks only later.
  const auto later = flows.Start(kOne, kPage, true, true, now);
  assert(flows.Bind(kOne, later, 4));
  assert(flows.Navigating(4, "https://" + kOne + ".chromiumapp.org/", now));
  Told late;
  assert(flows.Wait(kOne, later, late.waiter()));
  assert(late.said == std::vector<std::string>{"https://" + kOne + ".chromiumapp.org/"});
  // Asked once: it is gone after.
  assert(!flows.Wait(kOne, later, late.waiter()));

  // Another extension may not ask for it.
  const auto own = flows.Start(kOne, kPage, true, true, now);
  assert(!flows.Wait(kTwo, own, late.waiter()));
  // The person closes the panel: refused, in Chrome's words.
  assert(flows.Bind(kOne, own, 5));
  Told refused;
  assert(flows.Wait(kOne, own, refused.waiter()));
  flows.Closed(5, now);
  assert(refused.said == std::vector<std::string>{Error(tilo::kSignInRefused)});

  // No tab took it up: the Browser could not show it, and a tab that
  // comes too late is no sign-in's. A tab bound in time is not undone.
  const auto unshown = flows.Start(kTwo, kPage, true, true, now);
  Told nowhere;
  assert(flows.Wait(kTwo, unshown, nowhere.waiter()));
  flows.Unshown(unshown, now);
  assert(nowhere.said == std::vector<std::string>{Error(tilo::kSignInNotShown)});
  assert(!flows.Bind(kTwo, unshown, 6));
  const auto shown = flows.Start(kTwo, kPage, true, true, now);
  assert(flows.Bind(kTwo, shown, 6));
  flows.Unshown(shown, now);
  assert(flows.PageOf(6));

  // A second sign-in shown to the person ends the first, whose panel
  // closes.
  closed.clear();
  Told replaced;
  assert(flows.Wait(kTwo, shown, replaced.waiter()));
  const auto again = flows.Start(kTwo, "https://accounts.example/other", true, true, now);
  assert(replaced.said == std::vector<std::string>{Error(tilo::kSignInRefused)});
  assert(closed == std::vector<int>{6});
  assert(!flows.PageOf(6));
  assert(flows.Bind(kTwo, again, 7));
  assert(flows.PageOf(7) == "https://accounts.example/other");

  // One out of sight goes on beside it. Its page loading without sending
  // the person back ends it as needing them, and its tab closes; the blank
  // page a tab starts with is no page of the site.
  closed.clear();
  const auto quiet = flows.Start(kTwo, kPage, false, true, now);
  assert(!flows.Interactive(quiet));
  assert(flows.PageOf(7));
  assert(flows.Bind(kTwo, quiet, 8));
  Told needs;
  assert(flows.Wait(kTwo, quiet, needs.waiter()));
  flows.Loaded(8, "about:blank", now);
  assert(needs.said.empty());
  flows.Loaded(8, kPage, now);
  assert(needs.said == std::vector<std::string>{Error(tilo::kSignInInteraction)});
  assert(closed == std::vector<int>{8});
  // One that goes straight back ends with the address.
  const auto back = flows.Start(kTwo, kPage, false, true, now);
  assert(flows.Bind(kTwo, back, 9));
  assert(flows.Navigating(9, "https://" + kTwo + ".chromiumapp.org/cb#token=1", now));
  Told quick;
  assert(flows.Wait(kTwo, back, quick.waiter()));
  assert(quick.said == std::vector<std::string>{"https://" + kTwo + ".chromiumapp.org/cb#token=1"});
  // One whose site's script sends the person back waits past the load,
  // until its time is up; its tab closing ends it as needing them too.
  const auto scripted = flows.Start(kTwo, kPage, false, false, now);
  assert(flows.Bind(kTwo, scripted, 10));
  flows.Loaded(10, kPage, now);
  assert(flows.PageOf(10));
  closed.clear();
  flows.Expire(scripted, now);
  assert(closed == std::vector<int>{10});
  Told expired;
  assert(flows.Wait(kTwo, scripted, expired.waiter()));
  assert(expired.said == std::vector<std::string>{Error(tilo::kSignInInteraction)});
  const auto dropped = flows.Start(kTwo, kPage, false, true, now);
  assert(flows.Bind(kTwo, dropped, 11));
  flows.Closed(11, now);
  Told gone;
  assert(flows.Wait(kTwo, dropped, gone.waiter()));
  assert(gone.said == std::vector<std::string>{Error(tilo::kSignInInteraction)});
  // A sign-in shown to the person is not one to expire.
  flows.Expire(again, now);
  assert(flows.PageOf(7));

  // The extension is removed: its sign-in ends and its panel closes.
  closed.clear();
  Told removed;
  assert(flows.Wait(kTwo, again, removed.waiter()));
  flows.Forget(kTwo);
  assert(removed.said == std::vector<std::string>{Error(tilo::kSignInRefused)});
  assert(closed == std::vector<int>{7});

  // A few at once at most: the oldest ends for a new one.
  closed.clear();
  std::vector<uint64_t> many;
  for (size_t i = 0; i < ExtensionSignIn::kMaxOpen; ++i) {
    many.push_back(flows.Start(kOne, kPage, false, true, now));
    assert(flows.Bind(kOne, many.back(), 100 + static_cast<int>(i)));
  }
  flows.Start(kOne, kPage, false, true, now);
  assert(closed == std::vector<int>{100});
  assert(!flows.PageOf(100) && flows.PageOf(101));
  flows.Forget(kOne);
  assert(flows.size() == 0);

  // An outcome no one asks for is dropped a while later.
  const auto forgotten = flows.Start(kOne, kPage, true, true, now);
  assert(flows.Bind(kOne, forgotten, 12));
  flows.Closed(12, now);
  assert(flows.size() == 1);
  flows.Start(kTwo, kPage, true, true, now + std::chrono::minutes(2));
  Told late_ask;
  assert(!flows.Wait(kOne, forgotten, late_ask.waiter()));
  return 0;
}
