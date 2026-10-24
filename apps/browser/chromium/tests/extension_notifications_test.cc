#include "extension_notifications.h"
#include <cassert>
#include <string>
#include <vector>

using tilo::ExtensionNotifications;
using tilo::NotificationOptions;

static NotificationOptions Basic(const std::string& title) {
  NotificationOptions options;
  options.type = "basic";
  options.icon = "icon.png";
  options.title = title;
  options.message = "A message";
  return options;
}

int main() {
  ExtensionNotifications notifications;
  const std::string one(32, 'a'), two(32, 'b');

  // Created under the id given, or one made up; listed oldest first, each
  // extension its own.
  auto created = notifications.Create(one, "first", Basic("First"));
  assert(!created.error && created.id == "first");
  auto made = notifications.Create(one, "", Basic("Second"));
  assert(!made.error && made.id.size() == 36 && made.id != "first");
  assert((notifications.All(one) == std::vector<std::string>{"first", made.id}));
  assert(notifications.All(two).empty());
  assert(notifications.Find(one, "first")->title == "First");
  assert(!notifications.Find(two, "first"));

  // Created again under its id: replaced, and now the newest.
  assert(!notifications.Create(one, "first", Basic("Again")).error);
  assert(notifications.Find(one, "first")->title == "Again");
  assert((notifications.All(one) == std::vector<std::string>{made.id, "first"}));

  // Chrome's required properties.
  for (int missing = 0; missing < 4; ++missing) {
    auto options = Basic("Missing");
    if (missing == 0) options.type.reset();
    if (missing == 1) options.icon.reset();
    if (missing == 2) options.title.reset();
    if (missing == 3) options.message.reset();
    auto refused = notifications.Create(one, "missing", options);
    assert(refused.error && *refused.error == "Some of the required properties are missing: type, iconUrl, title and message.");
  }
  assert(!notifications.Find(one, "missing"));

  // A progress notification has a whole number from 0 to 100; no other
  // type has one.
  auto progress = Basic("Copying");
  progress.type = "progress";
  assert(!notifications.Create(one, "copy", progress).error);
  assert(notifications.Find(one, "copy")->progress == 0);
  progress.progress = 40;
  assert(!notifications.Create(one, "copy", progress).error);
  assert(notifications.Find(one, "copy")->progress == 40);
  for (double wrong : {-1.0, 101.0, 2.5}) {
    progress.progress = wrong;
    assert(*notifications.Create(one, "wrong", progress).error == "The progress value should range from 0 to 100");
  }
  auto basic = Basic("Plain");
  basic.progress = 10;
  assert(*notifications.Create(one, "plain", basic).error ==
         "The progress value should not be specified for non-progress notification");
  basic = Basic("Plain");
  basic.items = std::vector<tilo::NotificationItem>{{"a", "b"}};
  assert(*notifications.Create(one, "plain", basic).error == "List items provided for notification type != list");
  basic = Basic("Plain");
  basic.image = "image.png";
  assert(*notifications.Create(one, "plain", basic).error == "Image resource provided for notification type != image");
  basic = Basic("Plain");
  basic.type = "toast";
  assert(notifications.Create(one, "plain", basic).error);

  // At most two buttons, as Chrome shows.
  auto buttons = Basic("Buttons");
  buttons.buttons = std::vector<std::string>{"Yes", "No", "Maybe"};
  assert(!notifications.Create(one, "ask", buttons).error);
  assert((notifications.Find(one, "ask")->buttons == std::vector<std::string>{"Yes", "No"}));

  // An update changes what it names of one still shown, and keeps the rest;
  // one not shown is not made.
  NotificationOptions update;
  update.progress = 75;
  auto updated = notifications.Update(one, "copy", update);
  assert(!updated.error && updated.updated);
  assert(notifications.Find(one, "copy")->progress == 75 && notifications.Find(one, "copy")->title == "Copying");
  assert(!notifications.Update(one, "nothing", update).updated);
  assert(!notifications.Find(one, "nothing"));
  update.progress = 150;
  assert(notifications.Update(one, "copy", update).error);
  assert(notifications.Find(one, "copy")->progress == 75);
  // Becoming a basic one, it has no progress.
  NotificationOptions retype;
  retype.type = "basic";
  assert(notifications.Update(one, "copy", retype).updated);
  assert(!notifications.Find(one, "copy")->progress);

  // Cleared: whether it was shown.
  assert(notifications.Clear(one, "copy"));
  assert(!notifications.Clear(one, "copy"));
  assert(!notifications.Clear(two, "first"));

  // Text is bounded, never cut inside a character.
  std::string long_title(ExtensionNotifications::kMaxText - 1, 'x');
  long_title += "\xc3\xa9\xc3\xa9";
  assert(!notifications.Create(one, "long", Basic(long_title)).error);
  assert(notifications.Find(one, "long")->title.size() == ExtensionNotifications::kMaxText - 1);
  assert(notifications.Create(one, std::string(ExtensionNotifications::kMaxId + 1, 'i'), Basic("Long id")).error);

  // An extension that leaves takes its notifications with it.
  assert(!notifications.Create(two, "theirs", Basic("Theirs")).error);
  const auto gone = notifications.Forget(one);
  assert(!gone.empty() && notifications.All(one).empty());
  assert((notifications.All(two) == std::vector<std::string>{"theirs"}));

  // One extension shows a bounded number at once; replacing one is not
  // another.
  for (size_t i = 0; i < ExtensionNotifications::kMaxShown; ++i)
    assert(!notifications.Create(one, std::to_string(i), Basic("Many")).error);
  assert(notifications.Create(one, "more", Basic("Many")).error);
  assert(!notifications.Create(one, "0", Basic("Again")).error);
  return 0;
}
