#pragma once
// Extensions' notifications (`chrome.notifications`).
//
// Chrome shows an extension's notifications itself; here Tilo's
// notification service shows them, through the Browser, so the host keeps
// what each extension has shown (getAll, update, clear) and checks it as
// Chrome does:
//
// - `create` needs a type, an icon, a title and a message; an empty id gets
//   one made up, and an id already shown is replaced.
// - Only a progress notification has a progress (0 to 100), only a list
//   notification items, only an image notification an image.
// - At most two buttons, as Chrome shows; more are left out.
// - `update` changes what it names of a notification still shown, and
//   answers false for one that is not; `clear` answers whether it was shown.
//
// Icons and images are taken and not shown: Tilo shows the Browser's icon
// beside the extension's name.
#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <map>
#include <optional>
#include <random>
#include <string>
#include <utility>
#include <vector>

namespace tilo {

// One item of a list notification.
struct NotificationItem {
  std::string title;
  std::string message;
  bool operator==(const NotificationItem& other) const { return title == other.title && message == other.message; }
};

// What a call names of a notification: each field only if given.
struct NotificationOptions {
  std::optional<std::string> type, icon, title, message, context, image;
  std::optional<std::vector<NotificationItem>> items;
  std::optional<std::vector<std::string>> buttons;
  // A number the call gave, which must be a whole one from 0 to 100.
  std::optional<double> progress;
  std::optional<bool> require_interaction, silent;
};

// A notification as it is shown.
struct ExtensionNotification {
  std::string type, title, message, context;
  bool icon = false;
  std::vector<NotificationItem> items;
  std::vector<std::string> buttons;
  // From 0 to 100 for a progress notification, else none.
  std::optional<int> progress;
  bool require_interaction = false, silent = false;
};

class ExtensionNotifications {
 public:
  // At most this many shown at once by one extension, and the longest id.
  static constexpr size_t kMaxShown = 256;
  static constexpr size_t kMaxId = 500;
  static constexpr size_t kMaxText = 4096;
  static constexpr size_t kMaxItems = 16;
  static constexpr size_t kMaxButtons = 2;

  // Show a notification of `extension` under `id` (one made up if empty):
  // the id, or why not.
  struct Created {
    std::optional<std::string> error;
    std::string id;
  };
  Created Create(const std::string& extension, std::string id, const NotificationOptions& options) {
    if (!options.type || !options.icon || !options.title || !options.message)
      return {std::string("Some of the required properties are missing: type, iconUrl, title and message."), {}};
    if (id.size() > kMaxId) return {std::string("The notification id is too long."), {}};
    auto& shown = shown_[extension];
    if (id.empty()) {
      do id = MadeUpId(); while (shown.count(id));
    }
    if (!shown.count(id) && shown.size() >= kMaxShown) return {std::string("Too many notifications are shown."), {}};
    ExtensionNotification notification;
    if (auto error = Apply(notification, options)) return {error, {}};
    shown[id] = std::move(notification);
    order_[extension].erase(std::remove(order_[extension].begin(), order_[extension].end(), id), order_[extension].end());
    order_[extension].push_back(id);
    return {std::nullopt, id};
  }

  // Change what `options` names of the notification shown under `id`:
  // whether there was one, or why not.
  struct Updated {
    std::optional<std::string> error;
    bool updated = false;
  };
  Updated Update(const std::string& extension, const std::string& id, const NotificationOptions& options) {
    auto* found = Mutable(extension, id);
    if (!found) return {std::nullopt, false};
    auto changed = *found;
    if (auto error = Apply(changed, options)) return {error, false};
    *found = std::move(changed);
    return {std::nullopt, true};
  }

  // Take the notification under `id` away: whether one was shown.
  bool Clear(const std::string& extension, const std::string& id) {
    auto found = shown_.find(extension);
    if (found == shown_.end() || !found->second.erase(id)) return false;
    auto& order = order_[extension];
    order.erase(std::remove(order.begin(), order.end(), id), order.end());
    if (found->second.empty()) { shown_.erase(found); order_.erase(extension); }
    return true;
  }

  // The ids the extension shows, oldest first.
  std::vector<std::string> All(const std::string& extension) const {
    auto found = order_.find(extension);
    return found == order_.end() ? std::vector<std::string>() : found->second;
  }

  const ExtensionNotification* Find(const std::string& extension, const std::string& id) const {
    auto found = shown_.find(extension);
    if (found == shown_.end()) return nullptr;
    auto notification = found->second.find(id);
    return notification == found->second.end() ? nullptr : &notification->second;
  }

  // The extension left: the ids it showed, which go with it.
  std::vector<std::string> Forget(const std::string& extension) {
    auto ids = All(extension);
    shown_.erase(extension);
    order_.erase(extension);
    return ids;
  }

 private:
  ExtensionNotification* Mutable(const std::string& extension, const std::string& id) {
    return const_cast<ExtensionNotification*>(Find(extension, id));
  }

  static std::string Bounded(const std::string& text) {
    if (text.size() <= kMaxText) return text;
    // Cut at a character's start, never inside one.
    size_t end = kMaxText;
    while (end > 0 && (static_cast<unsigned char>(text[end]) & 0xC0) == 0x80) --end;
    return text.substr(0, end);
  }

  // `options` laid over `notification`, as Chrome checks the result.
  static std::optional<std::string> Apply(ExtensionNotification& notification, const NotificationOptions& options) {
    if (options.type) {
      const auto& type = *options.type;
      if (type != "basic" && type != "image" && type != "list" && type != "progress")
        return std::string("Invalid notification type: ") + type + ".";
      notification.type = type;
    }
    if (options.icon) notification.icon = !options.icon->empty();
    if (options.title) notification.title = Bounded(*options.title);
    if (options.message) notification.message = Bounded(*options.message);
    if (options.context) notification.context = Bounded(*options.context);
    if (options.items) {
      notification.items.clear();
      for (const auto& item : *options.items) {
        if (notification.items.size() >= kMaxItems) break;
        notification.items.push_back({Bounded(item.title), Bounded(item.message)});
      }
    }
    if (options.buttons) {
      notification.buttons.clear();
      for (const auto& button : *options.buttons) {
        if (notification.buttons.size() >= kMaxButtons) break;
        notification.buttons.push_back(Bounded(button));
      }
    }
    if (options.require_interaction) notification.require_interaction = *options.require_interaction;
    if (options.silent) notification.silent = *options.silent;
    const bool progress_type = notification.type == "progress";
    if (options.progress) {
      if (!progress_type) return std::string("The progress value should not be specified for non-progress notification");
      const double value = *options.progress;
      if (!(value >= 0 && value <= 100) || value != static_cast<double>(static_cast<int>(value)))
        return std::string("The progress value should range from 0 to 100");
      notification.progress = static_cast<int>(value);
    }
    if (progress_type && !notification.progress) notification.progress = 0;
    if (!progress_type) notification.progress.reset();
    if (notification.type != "list" && options.items && !options.items->empty())
      return std::string("List items provided for notification type != list");
    if (notification.type != "list") notification.items.clear();
    if (notification.type != "image" && options.image && !options.image->empty())
      return std::string("Image resource provided for notification type != image");
    return std::nullopt;
  }

  // An id for a notification created without one, as Chrome makes one.
  std::string MadeUpId() {
    static const char digits[] = "0123456789abcdef";
    std::uniform_int_distribution<int> digit(0, 15);
    std::string id;
    for (int i = 0; i < 32; ++i) {
      if (i == 8 || i == 12 || i == 16 || i == 20) id += '-';
      id += digits[digit(random_)];
    }
    return id;
  }

  std::map<std::string, std::map<std::string, ExtensionNotification>> shown_;
  std::map<std::string, std::vector<std::string>> order_;
  std::mt19937_64 random_{std::random_device{}()};
};

}  // namespace tilo
