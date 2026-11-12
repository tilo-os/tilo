#include "http_auth.h"
#include <cassert>
#include <string>

int main() {
  using tilo::AuthAsker;
  using tilo::AuthMayAsk;
  using tilo::AuthRequest;

  // Who asks is the origin the credentials go to, as a person reads it: the
  // scheme always, the port unless it is the scheme's own.
  assert(AuthAsker({"https", "example.org", 0}, false, "example.org", 443) == "https://example.org");
  assert(AuthAsker({"https", "example.org", 443}, false, "example.org", 443) == "https://example.org");
  assert(AuthAsker({"http", "127.0.0.1", 8080}, false, "127.0.0.1", 8080) == "http://127.0.0.1:8080");
  assert(AuthAsker({"HTTPS", "Example.ORG", 8443}, false, "example.org", 8443) == "https://example.org:8443");
  assert(AuthAsker({"http", "[::1]", 8080}, false, "::1", 8080) == "http://[::1]:8080");
  // A challenge that is not the request's own server names nobody: the
  // credentials would go to a host other than the one shown.
  assert(AuthAsker({"https", "example.org", 0}, false, "evil.example", 443).empty());
  assert(AuthAsker({"https", "example.org", 0}, false, "example.org", 8443).empty());
  assert(AuthAsker({"http", "example.org", 0}, false, "example.org", 443).empty());
  // Only a web address asks.
  assert(AuthAsker({"ftp", "example.org", 0}, false, "example.org", 21).empty());
  assert(AuthAsker({"wss", "example.org", 0}, false, "example.org", 443).empty());
  assert(AuthAsker({"https", "", 0}, false, "", 443).empty());
  assert(AuthAsker({"https", "example.org", 0}, false, "example.org", 0).empty());
  assert(AuthAsker({"https", "example.org", 70000}, false, "example.org", 443).empty());
  // A proxy is named by its own host and port, whatever was asked for.
  assert(AuthAsker({"https", "example.org", 0}, true, "Proxy.example", 3128) == "proxy.example:3128");
  assert(AuthAsker({"https", "example.org", 0}, true, "", 3128).empty());
  assert(AuthAsker({"https", "example.org", 0}, true, "proxy.example", 0).empty());

  const std::string shop = "https://shop.example", bank = "https://bank.example";
  // The tab's own document asks, on its way there: by its address, without
  // its fragment, whatever the tab shows now.
  assert(AuthMayAsk("https://bank.example/login", bank, false, "https://bank.example/login", shop));
  assert(AuthMayAsk("https://bank.example/login", bank, false, "https://bank.example/login#top", ""));
  assert(AuthMayAsk("https://bank.example/login#a", bank, false, "https://bank.example/login", ""));
  // What a document loads from its own origin asks.
  assert(AuthMayAsk("https://shop.example/api/orders", shop, false, "", shop));
  assert(AuthMayAsk("https://shop.example/api/orders", shop, false, "https://shop.example/next", shop));
  // What it loads from another origin never does: not an image, a frame or
  // a request, and not while the tab is on its way somewhere else.
  assert(!AuthMayAsk("https://bank.example/pixel.png", bank, false, "", shop));
  assert(!AuthMayAsk("https://bank.example/pixel.png", bank, false, "https://shop.example/next", shop));
  assert(!AuthMayAsk("https://bank.example/login/other", bank, false, "https://bank.example/login", shop));
  assert(!AuthMayAsk("http://shop.example/api", "http://shop.example", false, "", shop));
  assert(!AuthMayAsk("https://shop.example:8443/api", "https://shop.example:8443", false, "", shop));
  // A document without an origin (a blank page, a data page) loads nothing
  // that asks.
  assert(!AuthMayAsk("https://bank.example/pixel.png", bank, false, "", ""));
  // Nobody to name, nobody asked.
  assert(!AuthMayAsk("https://bank.example/login", "", false, "https://bank.example/login", bank));
  assert(!AuthMayAsk("https://bank.example/login", "", true, "", ""));
  // A proxy asks whatever the page is: no page chose it.
  assert(AuthMayAsk("https://bank.example/pixel.png", "proxy.example:3128", true, "", shop));

  // The realm is the server's text: shown without control characters,
  // trimmed and bounded, never cut inside a character.
  assert(tilo::AuthRealm("  Staff room \r\n") == "Staff room");
  assert(tilo::AuthRealm("Staff\nroom\x7f") == "Staff room");
  assert(tilo::AuthRealm("\t\n ").empty());
  assert(tilo::AuthRealm(std::string(1000, 'x')).size() == tilo::kMaxAuthRealm);
  std::string umlauts;
  for (int i = 0; i < 200; ++i) umlauts += "\xC3\xA4";
  const auto realm = tilo::AuthRealm("x" + umlauts);
  assert(realm.size() == tilo::kMaxAuthRealm - 1 && realm.back() == '\xA4');

  // The scheme is a plain token, in lower case.
  assert(tilo::AuthScheme("Basic") == "basic");
  assert(tilo::AuthScheme("digest") == "digest");
  assert(tilo::AuthScheme("").empty());
  assert(tilo::AuthScheme("basic realm").empty());
  assert(tilo::AuthScheme(std::string(64, 'a')).empty());

  // One question is one asker, realm and scheme; anything else is another.
  const auto question = tilo::AuthQuestion(shop, false, "Staff room", "basic");
  assert(question == tilo::AuthQuestion(shop, false, "Staff room", "basic"));
  assert(question != tilo::AuthQuestion(bank, false, "Staff room", "basic"));
  assert(question != tilo::AuthQuestion(shop, false, "Vault", "basic"));
  assert(question != tilo::AuthQuestion(shop, false, "Staff room", "digest"));
  assert(question != tilo::AuthQuestion(shop, true, "Staff room", "basic"));
  return 0;
}
