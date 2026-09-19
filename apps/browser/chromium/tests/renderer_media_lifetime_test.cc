#include "renderer_media_lifetime.h"
#include <cassert>
#include <map>
#include <memory>
#include <string>

struct Browser {
  int identifier;
  int GetIdentifier() const { return identifier; }
  bool IsSame(const std::shared_ptr<Browser>& other) const { return this == other.get(); }
};
struct Context { std::shared_ptr<Browser> browser; int token; };
int main() {
  auto old = std::make_shared<Browser>(Browser{7});
  auto replacement = std::make_shared<Browser>(Browser{7});
  auto unrelated = std::make_shared<Browser>(Browser{8});
  std::map<int, std::map<std::string, Context>> contexts{
      {7, {{"old-child", {old, 10}}, {"new-main", {replacement, 20}},
           {"new-child", {replacement, 21}}}},
      {8, {{"other-main", {unrelated, 30}}}}};
  qos::EraseMediaBrowserContexts(contexts, old);
  assert(contexts.contains(7));
  assert(contexts.at(7).size() == 2);
  assert(contexts.at(7).at("new-main").token == 20);
  assert(contexts.at(7).at("new-child").token == 21);
  assert(contexts.at(8).at("other-main").token == 30);
  // Repeated obsolete destruction must also preserve the replacement.
  qos::EraseMediaBrowserContexts(contexts, old);
  assert(contexts.at(7).size() == 2);
  qos::EraseMediaBrowserContexts(contexts, replacement);
  assert(!contexts.contains(7));
  assert(contexts.contains(8));
  qos::EraseMediaBrowserContexts(contexts, unrelated);
  assert(contexts.empty());
}
