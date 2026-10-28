#pragma once
// Extensions' context menu items (`chrome.contextMenus`).
//
// Chromium keeps an extension's items and adds them to the menus of its own
// windows; an offscreen page has neither, so its menu would never show them
// and a click on one could not name its tab. The host keeps them here
// instead: extension_tabs.js sends each create, update and remove, the
// host's page menu shows the items whose contexts match where the person
// clicked, and choosing one tells the extension (contextMenus.onClicked).
//
// The toolbar button's menu shows the items made for it (the "action"
// context), which the host tells the Browser of as they change.
//
// As in Chrome, the items outlast the extension's worker and a restart: a
// worker creates them once, when it is installed, so they are kept with the
// profile.
#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <map>
#include <optional>
#include <set>
#include <string>
#include <utility>
#include <vector>
#include "include/cef_parser.h"
#include "include/cef_values.h"
#include "match_pattern.h"

namespace tilo {

// Where the person opened a page's menu, as Chrome matches items to it.
struct MenuContext {
  std::string page, frame, link, src, selection, media;  // media: "image", "video", "audio" or empty
  bool editable = false;
  bool main_frame = true;
  // False for the toolbar button's menu, which is no page's.
  bool in_page = true;
};

// One item an extension's menu shows: a command, a checkbox or radio item
// with its state, a separator, or an item with a submenu of `children`.
struct MenuOffer {
  std::string extension, key, label;
  std::string type = "normal";  // "normal", "checkbox", "radio" or "separator"
  bool checked = false, enabled = true;
  std::vector<MenuOffer> children;
};

class ExtensionMenus {
 public:
  // Bounds on what one extension may keep.
  static constexpr size_t kMaxItems = 1000;
  static constexpr size_t kMaxTitle = 4096;
  static constexpr size_t kMaxPatterns = 100;
  static constexpr size_t kMaxId = 1024;
  // Chrome's longest item title in a menu, in characters.
  static constexpr size_t kMaxLabel = 75;
  // How deep submenus go, and Chrome's limit on the items at the top of a
  // toolbar button's menu (contextMenus.ACTION_MENU_TOP_LEVEL_LIMIT).
  static constexpr size_t kMaxDepth = 4;
  static constexpr size_t kMaxActionItems = 6;

  // Add an item; an error says why it was not.
  std::optional<std::string> Create(const std::string& extension, CefRefPtr<CefDictionaryValue> properties) {
    if (!properties || !properties->HasKey("id")) return std::string("An item needs an id");
    const auto id = properties->GetValue("id");
    if (id->GetType() != VTYPE_STRING && id->GetType() != VTYPE_INT && id->GetType() != VTYPE_DOUBLE) return std::string("Invalid id");
    const std::string key = Key(id);
    if (key.size() > kMaxId) return std::string("The id is too long");
    auto& items = items_[extension];
    if (Index(items, key)) return "Cannot create item with duplicate id " + Display(id);
    if (items.size() >= kMaxItems) return std::string("Too many menu items");
    Item item;
    item.key = key;
    item.id = id->Copy();
    if (auto error = Apply(extension, item, properties, true)) return error;
    if (item.type != "separator" && item.title.empty()) return std::string("All menu items except for separators must have a title.");
    items.push_back(std::move(item));
    SanitizeRadios(items);
    return std::nullopt;
  }
  std::optional<std::string> Update(const std::string& extension, CefRefPtr<CefValue> id, CefRefPtr<CefDictionaryValue> properties) {
    auto& items = items_[extension];
    auto index = id ? Index(items, Key(id)) : std::nullopt;
    if (!index) return "Cannot find menu item with id " + (id ? Display(id) : std::string("undefined"));
    Item item = items[*index];
    if (!properties) properties = CefDictionaryValue::Create();
    if (auto error = Apply(extension, item, properties, false)) return error;
    if (item.type != "separator" && item.title.empty()) return std::string("All menu items except for separators must have a title.");
    // Moved under another item: it comes last there, as in Chrome.
    const bool moved = item.parent != items[*index].parent;
    items[*index] = std::move(item);
    size_t at = *index;
    if (moved) {
      std::rotate(items.begin() + static_cast<std::ptrdiff_t>(at), items.begin() + static_cast<std::ptrdiff_t>(at) + 1, items.end());
      at = items.size() - 1;
    }
    // A radio item checked by an update is the one chosen in its group.
    if (items[at].type == "radio" && items[at].checked && properties->GetType("checked") == VTYPE_BOOL) CheckRadio(items, at);
    SanitizeRadios(items);
    return std::nullopt;
  }
  // Remove an item and everything under it.
  std::optional<std::string> Remove(const std::string& extension, CefRefPtr<CefValue> id) {
    auto& items = items_[extension];
    const std::string key = id ? Key(id) : std::string();
    if (!Index(items, key)) return "Cannot find menu item with id " + (id ? Display(id) : std::string("undefined"));
    std::set<std::string> gone{key};
    for (bool more = true; more;) {
      more = false;
      for (const auto& item : items)
        if (!item.parent.empty() && gone.count(item.parent) && gone.insert(item.key).second) more = true;
    }
    items.erase(std::remove_if(items.begin(), items.end(), [&](const Item& item) { return gone.count(item.key) > 0; }), items.end());
    SanitizeRadios(items);
    return std::nullopt;
  }
  void RemoveAll(const std::string& extension) { items_.erase(extension); }

  // The items of `extension` that apply where the menu opened, in order,
  // each with its submenu.
  std::vector<MenuOffer> Offers(const std::string& extension, const MenuContext& context) const {
    std::vector<MenuOffer> offers;
    auto found = items_.find(extension);
    if (found == items_.end()) return offers;
    Collect(extension, found->second, std::string(), &context, offers, 1);
    return offers;
  }
  // The items of `extension`'s toolbar button menu: at most six at its top.
  std::vector<MenuOffer> ActionOffers(const std::string& extension) const {
    std::vector<MenuOffer> offers;
    auto found = items_.find(extension);
    if (found == items_.end()) return offers;
    Collect(extension, found->second, std::string(), nullptr, offers, 1);
    size_t shown = 0;
    for (size_t i = 0; i < offers.size(); ++i) {
      if (offers[i].type == "separator" || ++shown <= kMaxActionItems) continue;
      offers.resize(i);
      break;
    }
    while (!offers.empty() && offers.back().type == "separator") offers.pop_back();
    return offers;
  }

  // The person chose the item: its state changes as Chrome's does (a
  // checkbox toggles, a radio item is chosen in its group) and the answer is
  // what `contextMenus.onClicked` is told, or null for an item now gone.
  CefRefPtr<CefDictionaryValue> Choose(const std::string& extension, const std::string& key, const MenuContext& context) {
    auto& items = items_[extension];
    auto index = Index(items, key);
    if (!index) return nullptr;
    Item& item = items[*index];
    if (!item.enabled || !item.visible || item.type == "separator") return nullptr;
    if (!context.in_page && !ForAction(item)) return nullptr;
    auto info = CefDictionaryValue::Create();
    info->SetValue("menuItemId", item.id->Copy());
    if (!item.parent.empty()) if (auto parent = Index(items, item.parent)) info->SetValue("parentMenuItemId", items[*parent].id->Copy());
    if (!context.media.empty()) info->SetString("mediaType", context.media);
    if (!context.link.empty()) info->SetString("linkUrl", context.link);
    if (!context.src.empty()) info->SetString("srcUrl", context.src);
    if (!context.page.empty()) info->SetString("pageUrl", context.page);
    if (!context.main_frame && !context.frame.empty()) info->SetString("frameUrl", context.frame);
    if (context.in_page && context.main_frame) info->SetInt("frameId", 0);
    if (!context.selection.empty()) info->SetString("selectionText", context.selection);
    info->SetBool("editable", context.editable);
    if (item.type == "checkbox" || item.type == "radio") {
      info->SetBool("wasChecked", item.checked);
      if (item.type == "checkbox") item.checked = !item.checked;
      else CheckRadio(items, *index);
      info->SetBool("checked", item.checked);
    }
    return info;
  }

  // The items of every extension, to keep with the profile, and back.
  std::string Save() const {
    auto all = CefDictionaryValue::Create();
    for (const auto& [extension, items] : items_) {
      if (items.empty()) continue;
      auto list = CefListValue::Create();
      for (const auto& item : items) list->SetDictionary(list->GetSize(), Properties(items, item));
      all->SetList(extension, list);
    }
    auto value = CefValue::Create();
    value->SetDictionary(all);
    return CefWriteJSON(value, JSON_WRITER_DEFAULT).ToString();
  }
  void Load(const std::string& json) {
    auto parsed = CefParseJSON(json, JSON_PARSER_RFC);
    if (!parsed || parsed->GetType() != VTYPE_DICTIONARY) return;
    auto all = parsed->GetDictionary();
    CefDictionaryValue::KeyList extensions;
    all->GetKeys(extensions);
    for (const auto& name : extensions) {
      auto list = all->GetType(name) == VTYPE_LIST ? all->GetList(name) : nullptr;
      std::vector<CefRefPtr<CefDictionaryValue>> left;
      for (size_t i = 0; list && i < list->GetSize() && i < kMaxItems; ++i)
        if (list->GetType(i) == VTYPE_DICTIONARY) left.push_back(list->GetDictionary(i)->Copy(false));
      // An item moved under one made after it comes back once its parent has.
      for (size_t before = 0; !left.empty() && left.size() != before;) {
        before = left.size();
        left.erase(std::remove_if(left.begin(), left.end(), [&](const auto& properties) {
          if (properties->HasKey("parentId") && !Index(items_[name.ToString()], Key(properties->GetValue("parentId")))) return false;
          Create(name.ToString(), properties);
          return true;
        }), left.end());
      }
      if (items_[name.ToString()].empty()) items_.erase(name.ToString());
    }
  }

 private:
  struct Item {
    std::string key;  // The id as JSON: a string and a number stay apart.
    CefRefPtr<CefValue> id;
    std::string parent;
    std::string type = "normal", title;
    std::vector<std::string> contexts{"page"};
    bool checked = false, enabled = true, visible = true;
    std::vector<std::string> documents, targets;
  };

  static std::string Key(CefRefPtr<CefValue> id) {
    if (id->GetType() == VTYPE_DOUBLE) {
      const double number = id->GetDouble();
      if (number == static_cast<double>(static_cast<int64_t>(number))) return std::to_string(static_cast<int64_t>(number));
    }
    return CefWriteJSON(id, JSON_WRITER_DEFAULT).ToString();
  }
  static std::string Display(CefRefPtr<CefValue> id) {
    return id->GetType() == VTYPE_STRING ? id->GetString().ToString() : Key(id);
  }
  static std::optional<size_t> Index(const std::vector<Item>& items, const std::string& key) {
    for (size_t i = 0; i < items.size(); ++i) if (items[i].key == key) return i;
    return std::nullopt;
  }

  static std::optional<std::vector<std::string>> Strings(CefRefPtr<CefDictionaryValue> properties, const char* key, size_t limit) {
    if (properties->GetType(key) != VTYPE_LIST) return std::nullopt;
    auto list = properties->GetList(key);
    std::vector<std::string> strings;
    for (size_t i = 0; i < list->GetSize() && strings.size() < limit; ++i)
      if (list->GetType(i) == VTYPE_STRING) strings.push_back(list->GetString(i).ToString());
    return strings;
  }

  // Apply what a create or update sets, checked as Chrome checks it.
  std::optional<std::string> Apply(const std::string& extension, Item& item, CefRefPtr<CefDictionaryValue> properties, bool creating) {
    static const std::set<std::string> types{"normal", "checkbox", "radio", "separator"};
    static const std::set<std::string> known{"all", "page", "frame", "selection", "link", "editable", "image", "video", "audio", "launcher", "browser_action", "page_action", "action"};
    if (properties->GetType("type") == VTYPE_STRING) {
      const std::string type = properties->GetString("type").ToString();
      if (!types.count(type)) return "Invalid type " + type;
      item.type = type;
    }
    if (properties->GetType("title") == VTYPE_STRING) {
      item.title = properties->GetString("title").ToString();
      if (item.title.size() > kMaxTitle) return std::string("The title is too long");
    }
    if (properties->GetType("checked") == VTYPE_BOOL) item.checked = properties->GetBool("checked");
    if (properties->GetType("enabled") == VTYPE_BOOL) item.enabled = properties->GetBool("enabled");
    if (properties->GetType("visible") == VTYPE_BOOL) item.visible = properties->GetBool("visible");
    if (auto contexts = Strings(properties, "contexts", known.size())) {
      for (const auto& context : *contexts) if (!known.count(context)) return "Invalid context " + context;
      if (contexts->empty()) return std::string("Value must have at least 1 items.");
      item.contexts = *contexts;
    }
    if (auto patterns = Strings(properties, "documentUrlPatterns", kMaxPatterns)) item.documents = *patterns;
    else if (properties->GetType("documentUrlPatterns") == VTYPE_NULL) item.documents.clear();
    if (auto patterns = Strings(properties, "targetUrlPatterns", kMaxPatterns)) item.targets = *patterns;
    else if (properties->GetType("targetUrlPatterns") == VTYPE_NULL) item.targets.clear();
    if (properties->HasKey("parentId") && properties->GetType("parentId") != VTYPE_NULL) {
      const std::string parent = Key(properties->GetValue("parentId"));
      const auto& items = items_[extension];
      const auto parent_index = Index(items, parent);
      if (!parent_index) return "Cannot find menu item with id " + Display(properties->GetValue("parentId"));
      if (items[*parent_index].type != "normal") return std::string("Parent items must have type \"normal\"");
      // An item may not end up under itself.
      for (std::string at = parent; !at.empty();) {
        if (at == item.key) return std::string("Cannot set an item to be its own ancestor");
        auto up = Index(items, at);
        at = up ? items[*up].parent : std::string();
      }
      item.parent = parent;
    } else if (!creating && properties->GetType("parentId") == VTYPE_NULL) {
      item.parent.clear();
    }
    if (item.type != "checkbox" && item.type != "radio") item.checked = false;
    // Items under this one need it to stay a normal item.
    if (!creating && item.type != "normal")
      for (const auto& other : items_[extension])
        if (other.parent == item.key) return std::string("Parent items must have type \"normal\"");
    return std::nullopt;
  }

  // The radio item at `index` is chosen: the others of its group, the run of
  // radio items it is in among its parent's items, are not.
  static void CheckRadio(std::vector<Item>& items, size_t index) {
    std::vector<size_t> siblings;
    for (size_t i = 0; i < items.size(); ++i) if (items[i].parent == items[index].parent) siblings.push_back(i);
    const size_t at = static_cast<size_t>(std::find(siblings.begin(), siblings.end(), index) - siblings.begin());
    for (size_t i = at; i > 0 && items[siblings[i - 1]].type == "radio"; --i) items[siblings[i - 1]].checked = false;
    for (size_t i = at + 1; i < siblings.size() && items[siblings[i]].type == "radio"; ++i) items[siblings[i]].checked = false;
    items[index].checked = true;
  }
  // Chrome's `SanitizeRadioList`: each group has exactly one item checked,
  // the last checked one, else its first.
  static void SanitizeRadios(std::vector<Item>& items) {
    std::map<std::string, std::vector<size_t>> by_parent;
    for (size_t i = 0; i < items.size(); ++i) by_parent[items[i].parent].push_back(i);
    for (const auto& [parent, siblings] : by_parent) {
      for (size_t start = 0; start < siblings.size();) {
        if (items[siblings[start]].type != "radio") { ++start; continue; }
        size_t end = start, last = siblings.size();
        for (; end < siblings.size() && items[siblings[end]].type == "radio"; ++end) {
          if (items[siblings[end]].checked) last = end;
          items[siblings[end]].checked = false;
        }
        items[siblings[last == siblings.size() ? start : last]].checked = true;
        start = end;
      }
    }
  }
  static CefRefPtr<CefDictionaryValue> Properties(const std::vector<Item>& items, const Item& item) {
    auto properties = CefDictionaryValue::Create();
    properties->SetValue("id", item.id->Copy());
    properties->SetString("type", item.type);
    properties->SetString("title", item.title);
    properties->SetBool("checked", item.checked);
    properties->SetBool("enabled", item.enabled);
    properties->SetBool("visible", item.visible);
    auto list = [](const std::vector<std::string>& strings) {
      auto out = CefListValue::Create();
      for (const auto& value : strings) out->SetString(out->GetSize(), value);
      return out;
    };
    properties->SetList("contexts", list(item.contexts));
    if (!item.documents.empty()) properties->SetList("documentUrlPatterns", list(item.documents));
    if (!item.targets.empty()) properties->SetList("targetUrlPatterns", list(item.targets));
    if (auto parent = item.parent.empty() ? std::nullopt : Index(items, item.parent)) properties->SetValue("parentId", items[*parent].id->Copy());
    return properties;
  }

  static bool Has(const std::vector<std::string>& contexts, const char* context) {
    return std::find(contexts.begin(), contexts.end(), context) != contexts.end();
  }
  // Chrome's `ExtensionContextAndPatternMatch`: an item shows for the
  // contexts it names, and "page" only where no other applies.
  static bool Applies(const Item& item, const MenuContext& context) {
    if (!item.visible) return false;
    if (!context.in_page) return ForAction(item);
    if (!item.documents.empty()) {
      const std::string& document = !context.main_frame && !context.frame.empty() ? context.frame : context.page;
      if (!AnyMatch(item.documents, document)) return false;
    }
    const auto& contexts = item.contexts;
    const bool link = !context.link.empty(), selection = !context.selection.empty();
    if (Has(contexts, "all") || (selection && Has(contexts, "selection")) || (context.editable && Has(contexts, "editable")) ||
        (!context.main_frame && Has(contexts, "frame")))
      return true;
    if (link && Has(contexts, "link") && (item.targets.empty() || AnyMatch(item.targets, context.link))) return true;
    if (!context.media.empty() && Has(contexts, context.media.c_str()) && (item.targets.empty() || AnyMatch(item.targets, context.src))) return true;
    return !link && !selection && !context.editable && context.media.empty() && Has(contexts, "page");
  }
  // Chrome's `MenuItemMatchesAction`: an item made for the toolbar button.
  static bool ForAction(const Item& item) {
    const auto& contexts = item.contexts;
    return item.visible && (Has(contexts, "all") || Has(contexts, "action") || Has(contexts, "browser_action") || Has(contexts, "page_action"));
  }
  static bool AnyMatch(const std::vector<std::string>& patterns, const std::string& url) {
    for (const auto& pattern : patterns) if (MatchPattern(pattern, url)) return true;
    return false;
  }

  // The items under `parent` that apply, as Chrome's
  // `RecursivelyAppendExtensionItems` lays them out: a disabled item shows
  // greyed, a run of radio items is set apart by separators, and a normal
  // item with items under it that apply opens a submenu of them. `context`
  // is null for the toolbar button's menu.
  void Collect(const std::string& extension, const std::vector<Item>& items, const std::string& parent, const MenuContext* context, std::vector<MenuOffer>& offers, size_t depth) const {
    static const std::string none;
    const std::string& selection = context ? context->selection : none;
    auto separate = [&] { if (!offers.empty() && offers.back().type != "separator") offers.push_back(MenuOffer{extension, std::string(), std::string(), "separator"}); };
    for (const auto& item : items) {
      if (item.parent != parent || !(context ? Applies(item, *context) : ForAction(item))) continue;
      if (item.type == "separator") { separate(); continue; }
      if ((item.type == "radio") != (!offers.empty() && offers.back().type == "radio")) {
        if (!offers.empty()) separate();
      }
      MenuOffer offer{extension, item.key, Label(item.title, selection), item.type, item.checked, item.enabled};
      if (item.type == "normal" && depth < kMaxDepth) Collect(extension, items, item.key, context, offer.children, depth + 1);
      offers.push_back(std::move(offer));
    }
    while (!offers.empty() && offers.back().type == "separator") offers.pop_back();
  }
  // The title as the menu shows it: `%s` stands for the selected text, and
  // a long one is cut short as Chrome cuts it.
  static std::string Label(const std::string& title, const std::string& selection) {
    std::string label;
    for (size_t i = 0; i < title.size(); ++i) {
      if (title[i] == '%' && i + 1 < title.size() && title[i + 1] == 's') { label += selection; ++i; }
      else label += title[i];
    }
    size_t characters = 0, cut = std::string::npos;
    for (size_t i = 0; i < label.size(); ++i) {
      if ((static_cast<unsigned char>(label[i]) & 0xC0) == 0x80) continue;
      if (characters == kMaxLabel - 1) cut = i;
      if (++characters > kMaxLabel) return label.substr(0, cut) + "\xE2\x80\xA6";
    }
    return label;
  }

  // Each extension's items, in the order it made them.
  std::map<std::string, std::vector<Item>> items_;
};

}  // namespace tilo
