#pragma once
// Tabs and windows for Chrome extensions.
//
// Chromium's extension APIs find a tab through the tab strips of its own
// windows. Tilo renders every page offscreen, in Alloy-style browsers no
// window holds, so `chrome.tabs.query` finds nothing, `scripting` and
// `tabs.sendMessage` find no tab by its id, and a toolbar click
// (`action.onClicked`) has nowhere to come from. Content scripts, messages from
// them (with `sender.tab`) and network rules work without help.
//
// The host fills the gap. A script (extension_tabs.js) in each extension
// service worker and extension page answers those APIs by asking the host:
// a request to a reserved address the host serves itself. A page of the
// extension is known by its address, its worker by a token the Browser wrote
// with the script into the worker (it runs first there); no page can show
// either. The host
// answers from its own tabs, within the access the extension was granted, and
// runs scripts in the extension's own isolated world of a page, where content
// scripts run, through the page's inspector, and captures a tab from the frame
// the host last painted of it. Events (a click, a tab loading)
// reach a service worker through the DevTools pipe, which wakes it if it
// sleeps; the host attaches only for as long as that takes, so an extension's
// worker still sleeps when idle.
#include <algorithm>
#include <cctype>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <deque>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <set>
#include <string>
#include <utility>
#include <vector>
#include "include/base/cef_callback.h"
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_image.h"
#include "include/cef_parser.h"
#include "include/cef_resource_handler.h"
#include "include/cef_scheme.h"
#include "include/cef_task.h"
#include "include/wrapper/cef_closure_task.h"
#include "extension_tabs_script.h"

namespace tilo {

inline constexpr char kExtensionBridge[] = "https://extension-bridge.tilo.invalid/";
inline constexpr size_t kMaxBridgeRequest = 8 * 1024 * 1024;

inline std::string ToJson(CefRefPtr<CefValue> value) {
  return CefWriteJSON(value, JSON_WRITER_DEFAULT).ToString();
}
inline CefRefPtr<CefValue> Wrap(CefRefPtr<CefDictionaryValue> dictionary) {
  auto value = CefValue::Create();
  if (dictionary) value->SetDictionary(dictionary); else value->SetNull();
  return value;
}
inline CefRefPtr<CefValue> Wrap(CefRefPtr<CefListValue> list) {
  auto value = CefValue::Create();
  value->SetList(list);
  return value;
}
inline std::optional<int> IntOf(CefRefPtr<CefValue> value) {
  if (!value) return std::nullopt;
  if (value->GetType() == VTYPE_INT) return value->GetInt();
  if (value->GetType() == VTYPE_DOUBLE) {
    const double number = value->GetDouble();
    if (number >= -2147483648.0 && number <= 2147483647.0 && number == static_cast<int>(number)) return static_cast<int>(number);
  }
  return std::nullopt;
}
inline std::optional<int> IntOf(CefRefPtr<CefDictionaryValue> dictionary, const char* key) {
  return dictionary && dictionary->HasKey(key) ? IntOf(dictionary->GetValue(key)) : std::nullopt;
}
inline std::string StringOf(CefRefPtr<CefDictionaryValue> dictionary, const char* key) {
  return dictionary && dictionary->GetType(key) == VTYPE_STRING ? dictionary->GetString(key).ToString() : std::string();
}
inline bool ExtensionId(const std::string& id) {
  if (id.size() != 32) return false;
  for (char c : id) if (c < 'a' || c > 'p') return false;
  return true;
}

// `*` matches any run of characters.
inline bool GlobMatch(const std::string& pattern, const std::string& text) {
  size_t p = 0, t = 0, star = std::string::npos, mark = 0;
  while (t < text.size()) {
    if (p < pattern.size() && pattern[p] == '*') { star = p++; mark = t; }
    else if (p < pattern.size() && pattern[p] == text[t]) { ++p; ++t; }
    else if (star != std::string::npos) { p = star + 1; t = ++mark; }
    else return false;
  }
  while (p < pattern.size() && pattern[p] == '*') ++p;
  return p == pattern.size();
}
// Chrome's match patterns (`<all_urls>`, `*://*.example.com/*`), for access to
// a page.
inline bool MatchPattern(const std::string& pattern, const std::string& url) {
  CefURLParts parts;
  if (!CefParseURL(url, parts)) return false;
  const std::string scheme = CefString(&parts.scheme).ToString();
  const std::string host = CefString(&parts.host).ToString();
  std::string path = CefString(&parts.path).ToString();
  const std::string query = CefString(&parts.query).ToString();
  if (!query.empty()) path += "?" + query;
  if (pattern == "<all_urls>") return scheme == "http" || scheme == "https" || scheme == "file" || scheme == "ws" || scheme == "wss";
  const auto separator = pattern.find("://");
  if (separator == std::string::npos) return false;
  const std::string want_scheme = pattern.substr(0, separator);
  const auto slash = pattern.find('/', separator + 3);
  const std::string want_host = pattern.substr(separator + 3, slash == std::string::npos ? std::string::npos : slash - separator - 3);
  const std::string want_path = slash == std::string::npos ? "/*" : pattern.substr(slash);
  if (want_scheme == "*" ? scheme != "http" && scheme != "https" : want_scheme != scheme) return false;
  if (want_host != "*" && scheme != "file") {
    if (want_host.rfind("*.", 0) == 0) {
      const std::string base = want_host.substr(2);
      if (host != base && !(host.size() > base.size() && host.compare(host.size() - base.size() - 1, std::string::npos, "." + base) == 0)) return false;
    } else if (want_host != host) {
      return false;
    }
  }
  return GlobMatch(want_path, path.empty() ? "/" : path);
}

// DevTools calls on one tab's inspector, each answered on a later UI turn.
class TabInspector final : public CefDevToolsMessageObserver {
 public:
  using Done = std::function<void(bool, CefRefPtr<CefDictionaryValue>)>;
  explicit TabInspector(CefRefPtr<CefBrowser> browser) : browser_(browser) {}
  void Call(const std::string& method, CefRefPtr<CefDictionaryValue> params, Done done) {
    if (!browser_) { Later(std::move(done)); return; }
    if (!registration_) registration_ = browser_->GetHost()->AddDevToolsMessageObserver(this);
    const int id = browser_->GetHost()->ExecuteDevToolsMethod(0, method, params);
    if (!id) { Later(std::move(done)); return; }
    pending_[id] = std::move(done);
  }
  // Each call of a binding the host added (Runtime.bindingCalled), for as
  // long as a listener is set.
  using Listener = std::function<void(CefRefPtr<CefDictionaryValue>)>;
  void Listen(Listener listener) {
    listener_ = std::move(listener);
    if (listener_ && !registration_ && browser_) registration_ = browser_->GetHost()->AddDevToolsMessageObserver(this);
  }
  // The execution contexts `Runtime.enable` reports, collected meanwhile.
  void Collect(bool on) { collecting_ = on; if (on) contexts_.clear(); }
  const std::vector<CefRefPtr<CefDictionaryValue>>& contexts() const { return contexts_; }
  void Detach() {
    registration_ = nullptr;
    browser_ = nullptr;
    listener_ = nullptr;
    auto pending = std::move(pending_);
    for (auto& [id, done] : pending) Later(std::move(done));
  }
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>, int id, bool success, const void* result, size_t size) override {
    // CEF may answer before ExecuteDevToolsMethod returns the id.
    std::string copy(static_cast<const char*>(result), size);
    CefPostTask(TID_UI, base::BindOnce([](CefRefPtr<TabInspector> self, int id, bool success, std::string copy) {
      self->Result(id, success, copy);
    }, CefRefPtr<TabInspector>(this), id, success, std::move(copy)));
  }
  void OnDevToolsEvent(CefRefPtr<CefBrowser>, const CefString& method, const void* params, size_t size) override {
    if (method == "Runtime.bindingCalled") {
      std::string copy(static_cast<const char*>(params), size);
      CefPostTask(TID_UI, base::BindOnce([](CefRefPtr<TabInspector> self, std::string copy) {
        auto parsed = CefParseJSON(copy, JSON_PARSER_RFC);
        if (self->listener_ && parsed && parsed->GetType() == VTYPE_DICTIONARY) self->listener_(parsed->GetDictionary());
      }, CefRefPtr<TabInspector>(this), std::move(copy)));
      return;
    }
    if (method != "Runtime.executionContextCreated") return;
    std::string copy(static_cast<const char*>(params), size);
    CefPostTask(TID_UI, base::BindOnce([](CefRefPtr<TabInspector> self, std::string copy) {
      if (!self->collecting_) return;
      auto parsed = CefParseJSON(copy, JSON_PARSER_RFC);
      if (parsed && parsed->GetType() == VTYPE_DICTIONARY && parsed->GetDictionary()->GetType("context") == VTYPE_DICTIONARY)
        self->contexts_.push_back(parsed->GetDictionary()->GetDictionary("context")->Copy(false));
    }, CefRefPtr<TabInspector>(this), std::move(copy)));
  }
  void OnDevToolsAgentDetached(CefRefPtr<CefBrowser>) override {
    auto pending = std::move(pending_);
    for (auto& [id, done] : pending) Later(std::move(done));
  }

 private:
  static void Later(Done done) {
    CefPostTask(TID_UI, base::BindOnce([](Done done) { done(false, nullptr); }, std::move(done)));
  }
  void Result(int id, bool success, const std::string& json) {
    auto found = pending_.find(id);
    if (found == pending_.end()) return;
    auto done = std::move(found->second);
    pending_.erase(found);
    auto parsed = CefParseJSON(json, JSON_PARSER_RFC);
    auto result = parsed && parsed->GetType() == VTYPE_DICTIONARY ? parsed->GetDictionary() : nullptr;
    if (pending_.empty() && !collecting_ && !listener_) registration_ = nullptr;
    done(success, result);
  }
  CefRefPtr<CefBrowser> browser_;
  CefRefPtr<CefRegistration> registration_;
  std::map<int, Done> pending_;
  bool collecting_ = false;
  std::vector<CefRefPtr<CefDictionaryValue>> contexts_;
  Listener listener_;
  IMPLEMENT_REFCOUNTING(TabInspector);
};

// One of the host's tabs, as extensions are told of it.
struct ExtensionTab {
  int tab = 0;           // The host's tab id.
  int id = 0;            // The id Chromium's extension APIs know it by.
  std::string url, title, favicon;
  bool loading = false;
  int width = 0, height = 0;
  // An extension's popup or settings page shown in a panel: no tab of its
  // own. Its "current window" is the page it was opened over.
  bool panel = false;
  int opener = 0;
};

// The frame the host last painted of a tab: BGRA, premultiplied, top row
// first, `width` by `height` device pixels.
struct ExtensionFrame {
  std::string pixels;
  int width = 0, height = 0;
};

// A captured tab as an image's data URL (`tabs.captureVisibleTab`), or empty
// when the frame cannot be encoded. Encoding a large frame takes a while:
// call it off the UI thread.
inline std::string EncodeFrame(const ExtensionFrame& frame, bool png, int quality) {
  auto image = CefImage::CreateImage();
  if (!image->AddBitmap(1.0f, frame.width, frame.height, CEF_COLOR_TYPE_BGRA_8888, CEF_ALPHA_TYPE_PREMULTIPLIED, frame.pixels.data(), frame.pixels.size()))
    return {};
  int width = 0, height = 0;
  // A tab is captured as it shows, opaque, as in Chrome.
  auto bytes = png ? image->GetAsPNG(1.0f, false, width, height) : image->GetAsJPEG(1.0f, quality, width, height);
  if (!bytes || !bytes->GetSize()) return {};
  std::string data(bytes->GetSize(), '\0');
  bytes->GetData(data.data(), data.size(), 0);
  return std::string(png ? "data:image/png;base64," : "data:image/jpeg;base64,") + CefBase64Encode(data.data(), data.size()).ToString();
}

class ExtensionTabs {
 public:
  using Reply = std::function<void(const std::string&)>;
  using PipeDone = std::function<void(bool, CefRefPtr<CefDictionaryValue>)>;
  struct Hooks {
    std::function<std::vector<ExtensionTab>()> tabs;
    // The page tab the person used last.
    std::function<int()> focused;
    std::function<CefRefPtr<TabInspector>(int)> inspector;
    // Whether the tab's inspector reports its contexts already (an agent's
    // log of it), so the host must not turn that off.
    std::function<bool(int)> keeps_contexts;
    std::function<void(int, const std::string&)> navigate;
    std::function<void(int, bool)> reload;
    std::function<void(int, const std::string&, bool)> open;
    std::function<void(int)> close;
    // A copy of the frame the tab shows, null when it shows none (hidden).
    std::function<std::shared_ptr<ExtensionFrame>(int)> frame;
    // A DevTools call on the browser's pipe, `session` empty for the browser.
    std::function<void(const std::string&, CefRefPtr<CefDictionaryValue>, const std::string&, PipeDone)> pipe;
    bool private_mode = false;
  };
  explicit ExtensionTabs(Hooks hooks) : hooks_(std::move(hooks)) {}

  // The Browser's own extension, whose content script is in every page and
  // whose worker answers which tab a page is (Identify).
  void SetIdentity(const std::string& id) { identity_ = id; }

  // What the extension may do, from its manifest as installed.
  // `token` is what its worker shows the bridge: the Browser wrote it with
  // the tab APIs into the extension's worker, which alone can read it.
  void SetAccess(const std::string& id, std::set<std::string> permissions, std::vector<std::string> hosts, const std::string& token) {
    access_[id] = Access{std::move(permissions), std::move(hosts)};
    for (auto it = tokens_.begin(); it != tokens_.end();) it = it->second == id ? tokens_.erase(it) : std::next(it);
    if (token.empty()) return;
    tokens_[token] = id;
    // What its worker asked before the Browser said whose token it is.
    for (auto it = held_.begin(); it != held_.end();) {
      if (it->token != token) { ++it; continue; }
      auto held = std::move(*it);
      it = held_.erase(it);
      Ensure([this, id, held] { Handle(id, 0, held.method, ToJson(Wrap(held.args)), held.reply); });
    }
  }
  void Forget(const std::string& id) {
    access_.erase(id);
    for (auto it = tokens_.begin(); it != tokens_.end();) it = it->second == id ? tokens_.erase(it) : std::next(it);
    listening_.erase(id);
    for (auto it = granted_.begin(); it != granted_.end();) it = it->first == id ? granted_.erase(it) : std::next(it);
    captures_.erase(id);
  }

  // A request to the bridge from `frame` of the host tab `caller`, or with
  // no tab (0) from a service worker: answered for the extension whose page
  // the frame shows, or whose worker holds the token the request carries,
  // and refused for anyone else.
  void Request(int caller, const std::string& frame, const std::string& method, const std::string& body, Reply reply) {
    auto parsed = CefParseJSON(body, JSON_PARSER_RFC);
    auto request = parsed && parsed->GetType() == VTYPE_DICTIONARY ? parsed->GetDictionary() : nullptr;
    if (!request) { Fail(reply, "Malformed request"); return; }
    std::string extension;
    const std::string prefix = "chrome-extension://";
    // Copied: it outlives the request it was read from.
    auto args = request->GetType("args") == VTYPE_DICTIONARY ? request->GetDictionary("args")->Copy(false) : CefDictionaryValue::Create();
    const auto token = StringOf(request, "token");
    if (caller && frame.rfind(prefix, 0) == 0) extension = frame.substr(prefix.size(), 32);
    else if (!caller) {
      auto found = tokens_.find(token);
      if (found != tokens_.end()) extension = found->second;
      else if (token.size() >= 32 && held_.size() < 256) {
        // A worker starts with the engine, before the Browser tells whose
        // token it carries: its first calls wait for that, briefly.
        held_.push_back(Held{token, method, args, reply});
        CefPostDelayedTask(TID_UI, base::BindOnce([](ExtensionTabs* self, std::string token) { self->Unheld(token); }, this, token), 10000);
        return;
      }
    }
    if (!ExtensionId(extension)) { Fail(reply, "Not an extension"); return; }
    Ensure([this, extension, caller, method, args, reply] { Handle(extension, caller, method, ToJson(Wrap(args)), reply); });
  }
  // Answer one request of an extension's script. `caller` is the host tab of
  // an extension page, 0 for its service worker.
  void Handle(const std::string& extension, int caller, const std::string& method, const std::string& body, Reply reply) {
    if (!access_.count(extension)) { Fail(reply, "This extension is not running"); return; }
    auto parsed = CefParseJSON(body, JSON_PARSER_RFC);
    auto args = parsed && parsed->GetType() == VTYPE_DICTIONARY ? parsed->GetDictionary() : CefDictionaryValue::Create();
    const auto tabs = Tabs();
    const auto* from = Find(tabs, caller);
    if (method == "listen") {
      const auto name = StringOf(args, "event");
      if (Listenable(name)) listening_[extension].insert(name);
      Succeed(reply, nullptr);
    } else if (method == "tabs.query") {
      auto list = CefListValue::Create();
      for (const auto& tab : tabs) if (!tab.panel && Matches(extension, tab, args, tabs, from)) list->SetDictionary(list->GetSize(), TabObject(extension, tab));
      Succeed(reply, Wrap(list));
    } else if (method == "tabs.get") {
      const auto* tab = ByExtensionId(tabs, IntOf(args, "tabId"));
      if (!tab) { Fail(reply, "No tab with id: " + std::to_string(IntOf(args, "tabId").value_or(0)) + "."); return; }
      Succeed(reply, Wrap(TabObject(extension, *tab)));
    } else if (method == "tabs.getCurrent") {
      Succeed(reply, from && !from->panel ? Wrap(TabObject(extension, *from)) : nullptr);
    } else if (method == "tabs.create" || method == "windows.create") {
      std::string url = StringOf(args, "url");
      if (method == "windows.create" && args->GetType("url") == VTYPE_LIST && args->GetList("url")->GetSize()) url = args->GetList("url")->GetString(0);
      if (url.empty()) url = "about:blank";
      const bool background = args->GetType("active") == VTYPE_BOOL && !args->GetBool("active");
      const int opener = from && !from->panel ? from->tab : hooks_.focused();
      Create(extension, opener, url, background, method == "windows.create", reply);
    } else if (method == "tabs.update") {
      auto properties = args->GetDictionary("properties");
      const auto* tab = IntOf(args, "tabId") ? ByExtensionId(tabs, IntOf(args, "tabId")) : Current(tabs, from);
      if (!tab) { Fail(reply, "No tab with id: " + std::to_string(IntOf(args, "tabId").value_or(0)) + "."); return; }
      const auto url = StringOf(properties, "url");
      if (!url.empty()) {
        if (!Navigable(extension, url)) { Fail(reply, "Cannot navigate to " + url); return; }
        hooks_.navigate(tab->tab, url);
      }
      Succeed(reply, Wrap(TabObject(extension, *tab)));
    } else if (method == "tabs.reload") {
      const auto* tab = IntOf(args, "tabId") ? ByExtensionId(tabs, IntOf(args, "tabId")) : Current(tabs, from);
      if (!tab) { Fail(reply, "No tab to reload"); return; }
      auto options = args->GetDictionary("options");
      hooks_.reload(tab->tab, options && options->GetType("bypassCache") == VTYPE_BOOL && options->GetBool("bypassCache"));
      Succeed(reply, nullptr);
    } else if (method == "tabs.remove") {
      auto ids = args->GetList("tabIds");
      for (size_t i = 0; ids && i < ids->GetSize(); ++i) {
        const auto* tab = ByExtensionId(tabs, IntOf(ids->GetValue(i)));
        if (!tab) { Fail(reply, "No tab with id: " + std::to_string(IntOf(ids->GetValue(i)).value_or(0)) + "."); return; }
      }
      for (size_t i = 0; ids && i < ids->GetSize(); ++i) hooks_.close(ByExtensionId(tabs, IntOf(ids->GetValue(i)))->tab);
      Succeed(reply, nullptr);
    } else if (method == "tabs.sendMessage") {
      const auto* tab = ByExtensionId(tabs, IntOf(args, "tabId"));
      if (!tab) { Fail(reply, "Could not establish connection. Receiving end does not exist."); return; }
      SendMessage(extension, *tab, args->GetValue("message"), reply);
    } else if (method == "scripting.executeScript" || method == "scripting.insertCSS") {
      auto target = args->GetDictionary("target");
      const auto* tab = ByExtensionId(tabs, IntOf(target, "tabId"));
      if (!tab) { Fail(reply, "No tab with id: " + std::to_string(IntOf(target, "tabId").value_or(0))); return; }
      if (!MayScript(extension, *tab)) { Fail(reply, "Cannot access contents of the page. Extension manifest must request permission to access the respective host."); return; }
      if (method == "scripting.insertCSS") InsertCss(extension, *tab, StringOf(args, "css"), reply);
      else ExecuteScript(extension, *tab, args, reply);
    } else if (method == "tabs.captureVisibleTab") {
      Capture(extension, tabs, from, args, reply);
    } else if (method == "windows.get" || method == "windows.getCurrent" || method == "windows.getLastFocused") {
      const ExtensionTab* tab = nullptr;
      if (method == "windows.get") tab = ByExtensionId(tabs, IntOf(args, "windowId"));
      else if (method == "windows.getCurrent") tab = Current(tabs, from);
      else tab = Find(tabs, hooks_.focused());
      if (!tab) { Fail(reply, "No window with id: " + std::to_string(IntOf(args, "windowId").value_or(-1)) + "."); return; }
      Succeed(reply, Wrap(WindowObject(extension, *tab, Populate(args))));
    } else if (method == "windows.getAll") {
      auto list = CefListValue::Create();
      for (const auto& tab : tabs) if (!tab.panel) list->SetDictionary(list->GetSize(), WindowObject(extension, tab, Populate(args)));
      Succeed(reply, Wrap(list));
    } else if (method == "windows.update") {
      const auto* tab = ByExtensionId(tabs, IntOf(args, "windowId"));
      if (!tab) { Fail(reply, "No window with id: " + std::to_string(IntOf(args, "windowId").value_or(-1)) + "."); return; }
      Succeed(reply, Wrap(WindowObject(extension, *tab, false)));
    } else {
      Fail(reply, "Tilo does not support " + method + " yet");
    }
  }

  // The extension's toolbar button opened its popup over `tab`, which it may
  // now script until the page navigates, as after a click.
  void Grant(const std::string& extension, int tab) {
    Ensure([this, extension, tab] {
      const auto tabs = Tabs();
      if (const auto* found = Find(tabs, tab); found && access_.count(extension)) granted_.insert({extension, found->id});
    });
  }

  // The person clicked the extension's toolbar button over `tab`: like
  // Chrome, it may now script that page until it navigates.
  void Clicked(const std::string& extension, int tab) {
    Ensure([this, extension, tab] { ClickedNow(extension, tab); });
  }
  void ClickedNow(const std::string& extension, int tab) {
    const auto tabs = Tabs();
    const auto* found = Find(tabs, tab);
    if (!found || !access_.count(extension)) return;
    granted_.insert({extension, found->id});
    auto args = CefListValue::Create();
    args->SetDictionary(0, TabObject(extension, *found));
    Push(extension, "action.onClicked", args, true);
  }

  // A tab's page changed: `status`, `url` or `title`.
  void Updated(int tab, CefRefPtr<CefDictionaryValue> change) {
    if (change->HasKey("url")) {
      // The worlds the host made went with the page.
      for (auto it = content_worlds_.begin(); it != content_worlds_.end();) it = it->first.first == tab ? content_worlds_.erase(it) : std::next(it);
      // Access a click gave ends with the page it was given for.
      const auto known = native_.find(tab);
      const int id = known != native_.end() ? known->second : 0;
      for (auto it = granted_.begin(); it != granted_.end();) it = it->second == id ? granted_.erase(it) : std::next(it);
    }
    if (!Listened("tabs.onUpdated")) return;
    Ensure([this, tab, change] { UpdatedNow(tab, change); });
  }
  void UpdatedNow(int tab, CefRefPtr<CefDictionaryValue> change) {
    const auto tabs = Tabs();
    const auto* found = Find(tabs, tab);
    if (!found || found->panel) return;
    for (const auto& [extension, events] : listening_) {
      if (!events.count("tabs.onUpdated")) continue;
      auto visible = change->Copy(false);
      if (!SeesContents(extension, *found)) { visible->Remove("url"); visible->Remove("title"); visible->Remove("favIconUrl"); }
      if (visible->GetSize() == 0) continue;
      auto args = CefListValue::Create();
      args->SetInt(0, found->id);
      args->SetDictionary(1, visible);
      args->SetDictionary(2, TabObject(extension, *found));
      Push(extension, "tabs.onUpdated", args, false);
    }
  }
  void Focused(int tab) {
    if (!Listened("tabs.onActivated") && !Listened("windows.onFocusChanged")) return;
    Ensure([this, tab] { FocusedNow(tab); });
  }
  void FocusedNow(int tab) {
    const auto tabs = Tabs();
    const auto* found = Find(tabs, tab);
    if (!found || found->panel) return;
    for (const auto& [extension, events] : listening_) {
      if (events.count("tabs.onActivated")) {
        auto info = CefDictionaryValue::Create();
        info->SetInt("tabId", found->id);
        info->SetInt("windowId", found->id);
        auto args = CefListValue::Create();
        args->SetDictionary(0, info);
        Push(extension, "tabs.onActivated", args, false);
      }
      if (events.count("windows.onFocusChanged")) {
        auto args = CefListValue::Create();
        args->SetInt(0, found->id);
        Push(extension, "windows.onFocusChanged", args, false);
      }
    }
  }
  void Created(int tab) {
    if (!Listened("tabs.onCreated")) return;
    Ensure([this, tab] { CreatedNow(tab); });
  }
  void CreatedNow(int tab) {
    const auto tabs = Tabs();
    const auto* found = Find(tabs, tab);
    if (!found || found->panel) return;
    for (const auto& [extension, events] : listening_) {
      if (!events.count("tabs.onCreated")) continue;
      auto args = CefListValue::Create();
      args->SetDictionary(0, TabObject(extension, *found));
      Push(extension, "tabs.onCreated", args, false);
    }
  }
  // Before the tab leaves the host's list.
  void Removed(int tab) {
    const auto tabs = Tabs();
    const auto* found = Find(tabs, tab);
    native_.erase(tab);
    for (auto it = content_worlds_.begin(); it != content_worlds_.end();) it = it->first.first == tab ? content_worlds_.erase(it) : std::next(it);
    if (!found || found->panel) return;
    for (auto it = granted_.begin(); it != granted_.end();) it = it->second == found->id ? granted_.erase(it) : std::next(it);
    for (const auto& [extension, events] : listening_) {
      if (!events.count("tabs.onRemoved")) continue;
      auto info = CefDictionaryValue::Create();
      info->SetInt("windowId", found->id);
      info->SetBool("isWindowClosing", true);
      auto args = CefListValue::Create();
      args->SetInt(0, found->id);
      args->SetDictionary(1, info);
      Push(extension, "tabs.onRemoved", args, false);
    }
  }
  // A page created a tab that opens `url`: answers an extension waiting on
  // the tab it asked for.
  void Opened(int tab, const std::string& url) {
    for (auto it = creating_.begin(); it != creating_.end(); ++it) {
      if (it->url != url && !(url.rfind(it->url, 0) == 0)) continue;
      const auto tabs = Tabs();
      const auto* found = Find(tabs, tab);
      if (!found) return;
      auto pending = std::move(*it);
      creating_.erase(it);
      auto object = TabObject(pending.extension, *found);
      Succeed(pending.reply, pending.window ? Wrap(WindowObject(pending.extension, *found, true)) : Wrap(object));
      return;
    }
  }

 private:
  struct Access { std::set<std::string> permissions; std::vector<std::string> hosts; };
  struct Creating { std::string extension, url; bool window; Reply reply; };
  struct Held { std::string token, method; CefRefPtr<CefDictionaryValue> args; Reply reply; };
  // Calls of a token no extension was given in time are no extension's.
  void Unheld(const std::string& token) {
    for (auto it = held_.begin(); it != held_.end();) {
      if (it->token != token) { ++it; continue; }
      auto reply = std::move(it->reply);
      it = held_.erase(it);
      Fail(reply, "Not an extension");
    }
  }
  using Answer = std::function<void(std::optional<std::string>, CefRefPtr<CefValue>)>;
  // Something to evaluate in an extension's worker, and who waits for it.
  struct Waiting { std::string expression; bool gesture; Answer answer; };

  static void Succeed(const Reply& reply, CefRefPtr<CefValue> result) {
    auto body = CefDictionaryValue::Create();
    if (result) body->SetValue("result", result); else body->SetNull("result");
    reply(ToJson(Wrap(body)));
  }
  static void Fail(const Reply& reply, const std::string& message) {
    auto body = CefDictionaryValue::Create();
    body->SetString("error", message);
    reply(ToJson(Wrap(body)));
  }
  static CefRefPtr<CefDictionaryValue> Session(const std::string& session) {
    auto params = CefDictionaryValue::Create();
    params->SetString("sessionId", session);
    return params;
  }
  bool Listened(const std::string& event) const {
    for (const auto& [extension, events] : listening_) if (events.count(event)) return true;
    return false;
  }
  // The host's tabs, each by the id Chromium's extension APIs know it by
  // once it was identified, by the browser's until then.
  std::vector<ExtensionTab> Tabs() const {
    auto tabs = hooks_.tabs();
    for (auto& tab : tabs) {
      auto found = native_.find(tab.tab);
      if (found != native_.end()) tab.id = found->second;
    }
    return tabs;
  }
  // Learn the extension id of every page tab not yet known, then go on.
  // Chromium gives it to a message from the page: one the Browser's own
  // extension sends from its world there and answers itself.
  void Ensure(std::function<void()> then) {
    std::vector<ExtensionTab> missing;
    if (!identity_.empty())
      for (const auto& tab : hooks_.tabs()) {
        const bool web = tab.url.rfind("http://", 0) == 0 || tab.url.rfind("https://", 0) == 0 || tab.url.rfind("file://", 0) == 0;
        if (!tab.panel && web && !native_.count(tab.tab)) missing.push_back(tab);
      }
    if (missing.empty()) { then(); return; }
    auto left = std::make_shared<size_t>(missing.size());
    auto shared = std::make_shared<std::function<void()>>(std::move(then));
    for (const auto& tab : missing) Identify(tab, [left, shared] { if (--*left == 0) (*shared)(); });
  }
  void Identify(const ExtensionTab& tab, std::function<void()> done) {
    auto& waiting = identifying_[tab.tab];
    waiting.push_back(std::move(done));
    if (waiting.size() > 1) return;
    const int key = tab.tab;
    auto finish = [this, key] {
      auto callbacks = std::move(identifying_[key]);
      identifying_.erase(key);
      for (auto& callback : callbacks) callback();
    };
    auto inspector = hooks_.inspector(tab.tab);
    if (!inspector) { finish(); return; }
    World(identity_, tab, [this, inspector, key, finish](int world, std::string) {
      if (!world) { finish(); return; }
      Evaluate(inspector, world, "chrome.runtime.sendMessage({tiloTabId: true})", [this, key, finish](std::optional<std::string> error, CefRefPtr<CefValue> value) {
        if (!error) if (auto id = IntOf(value)) native_[key] = *id;
        finish();
      });
    });
  }
  static bool Listenable(const std::string& name) {
    static const std::set<std::string> names{"action.onClicked", "tabs.onCreated", "tabs.onUpdated", "tabs.onActivated", "tabs.onRemoved", "windows.onFocusChanged"};
    return names.count(name) > 0;
  }
  static bool Populate(CefRefPtr<CefDictionaryValue> args) {
    auto query = args->GetDictionary("query");
    return query && query->GetType("populate") == VTYPE_BOOL && query->GetBool("populate");
  }
  static const ExtensionTab* Find(const std::vector<ExtensionTab>& tabs, int tab) {
    for (const auto& entry : tabs) if (entry.tab == tab) return &entry;
    return nullptr;
  }
  static const ExtensionTab* ByExtensionId(const std::vector<ExtensionTab>& tabs, std::optional<int> id) {
    if (!id) return nullptr;
    for (const auto& entry : tabs) if (entry.id == *id && !entry.panel) return &entry;
    return nullptr;
  }
  // The window a caller means by "current": its own tab's, the page a panel
  // was opened over, or for a service worker the one used last.
  const ExtensionTab* Current(const std::vector<ExtensionTab>& tabs, const ExtensionTab* from) const {
    if (from && !from->panel) return from;
    if (from && from->panel) if (const auto* opener = Find(tabs, from->opener)) return opener;
    return Find(tabs, hooks_.focused());
  }

  // Where an extension may send a tab, as in Chrome: a web page, a blank
  // one or its own; never the Browser's or another extension's pages, or a
  // script in place of a page.
  static bool Navigable(const std::string& extension, const std::string& url) {
    return url.rfind("http://", 0) == 0 || url.rfind("https://", 0) == 0 || url == "about:blank" ||
           url.rfind("chrome-extension://" + extension + "/", 0) == 0;
  }
  bool Has(const std::string& extension, const char* permission) const {
    auto found = access_.find(extension);
    return found != access_.end() && found->second.permissions.count(permission) > 0;
  }
  bool HostAccess(const std::string& extension, const std::string& url) const {
    auto found = access_.find(extension);
    if (found == access_.end()) return false;
    for (const auto& pattern : found->second.hosts) if (MatchPattern(pattern, url)) return true;
    return false;
  }
  bool Granted(const std::string& extension, const ExtensionTab& tab) const {
    return granted_.count({extension, tab.id}) > 0;
  }
  // Whether the extension may read the tab's address and title.
  bool SeesContents(const std::string& extension, const ExtensionTab& tab) const {
    return Has(extension, "tabs") || HostAccess(extension, tab.url) || Granted(extension, tab);
  }
  bool MayScript(const std::string& extension, const ExtensionTab& tab) const {
    return HostAccess(extension, tab.url) || Granted(extension, tab);
  }
  // Access to every site (`<all_urls>`, or any host of a scheme), which
  // with `activeTab` is what Chrome asks of an extension to capture a tab.
  bool AllSites(const std::string& extension) const {
    auto found = access_.find(extension);
    if (found == access_.end()) return false;
    if (found->second.permissions.count("<all_urls>")) return true;
    for (const auto& pattern : found->second.hosts) {
      if (pattern == "<all_urls>") return true;
      const auto separator = pattern.find("://");
      if (separator == std::string::npos) continue;
      const auto slash = pattern.find('/', separator + 3);
      if (pattern.substr(separator + 3, slash == std::string::npos ? std::string::npos : slash - separator - 3) == "*") return true;
    }
    return false;
  }

  // `tabs.captureVisibleTab`: the frame the window's tab shows, as Chrome
  // allows it, to an extension the person gave the tab by a click
  // (`activeTab`) or one with access to every site. Encoded off the UI
  // thread; at most twice a second for each extension, Chrome's quota, so
  // an extension cannot keep the host encoding.
  void Capture(const std::string& extension, const std::vector<ExtensionTab>& tabs, const ExtensionTab* from, CefRefPtr<CefDictionaryValue> args, Reply reply) {
    const auto window = IntOf(args, "windowId");
    if (args->HasKey("windowId") && args->GetType("windowId") != VTYPE_NULL && !window) { Fail(reply, "Invalid value for argument 1."); return; }
    // WINDOW_ID_CURRENT, or none: the caller's window.
    const auto* tab = window && *window != -2 ? ByExtensionId(tabs, window) : Current(tabs, from);
    if (!tab) { Fail(reply, "No window with id: " + std::to_string(window.value_or(-2)) + "."); return; }
    auto options = args->GetType("options") == VTYPE_DICTIONARY ? args->GetDictionary("options") : CefDictionaryValue::Create();
    bool png = false;
    if (options->GetType("format") == VTYPE_STRING) {
      const auto format = options->GetString("format").ToString();
      if (format != "png" && format != "jpeg") { Fail(reply, "Invalid value for argument 2. Property 'format': Value must be one of jpeg, png."); return; }
      png = format == "png";
    } else if (options->HasKey("format") && options->GetType("format") != VTYPE_NULL) {
      Fail(reply, "Invalid value for argument 2. Property 'format': Invalid type: expected string."); return;
    }
    int quality = 90;
    if (options->HasKey("quality") && options->GetType("quality") != VTYPE_NULL) {
      const auto given = IntOf(options, "quality");
      if (!given || *given < 0 || *given > 100) { Fail(reply, "Invalid value for argument 2. Property 'quality': Value must be an integer from 0 to 100."); return; }
      quality = *given;
    }
    const bool active = Has(extension, "activeTab") && Granted(extension, *tab);
    if (!active && !AllSites(extension)) { Fail(reply, "Either the '<all_urls>' or 'activeTab' permission is required."); return; }
    const bool own = tab->url.rfind("chrome-extension://" + extension + "/", 0) == 0;
    if (!active && !own && !HostAccess(extension, tab->url)) {
      Fail(reply, "Cannot access contents of the page. Extension manifest must request permission to access the respective host.");
      return;
    }
    auto& calls = captures_[extension];
    const auto now = std::chrono::steady_clock::now();
    while (!calls.empty() && now - calls.front() >= std::chrono::seconds(1)) calls.pop_front();
    if (calls.size() >= 2) { Fail(reply, "This request exceeds the MAX_CAPTURE_VISIBLE_TAB_CALLS_PER_SECOND quota."); return; }
    calls.push_back(now);
    auto frame = hooks_.frame ? hooks_.frame(tab->tab) : nullptr;
    if (!frame || frame->pixels.empty()) { Fail(reply, "Failed to capture tab: view is invisible"); return; }
    CefPostTask(TID_FILE_USER_BLOCKING, base::BindOnce([](std::shared_ptr<ExtensionFrame> frame, bool png, int quality, Reply reply) {
      auto url = EncodeFrame(*frame, png, quality);
      frame.reset();
      CefPostTask(TID_UI, base::BindOnce([](std::string url, Reply reply) {
        if (url.empty()) { Fail(reply, "Failed to capture tab: unknown error"); return; }
        auto value = CefValue::Create();
        value->SetString(url);
        Succeed(reply, value);
      }, std::move(url), std::move(reply)));
    }, std::move(frame), png, quality, std::move(reply)));
  }

  CefRefPtr<CefDictionaryValue> TabObject(const std::string& extension, const ExtensionTab& tab) const {
    auto object = CefDictionaryValue::Create();
    object->SetInt("id", tab.id);
    object->SetInt("windowId", tab.id);
    object->SetInt("index", 0);
    object->SetBool("active", true);
    object->SetBool("highlighted", true);
    object->SetBool("selected", true);
    object->SetBool("pinned", false);
    object->SetBool("audible", false);
    object->SetBool("discarded", false);
    object->SetBool("autoDiscardable", true);
    object->SetBool("frozen", false);
    object->SetBool("incognito", hooks_.private_mode);
    object->SetInt("groupId", -1);
    object->SetString("status", tab.loading ? "loading" : "complete");
    object->SetInt("width", tab.width);
    object->SetInt("height", tab.height);
    auto muted = CefDictionaryValue::Create();
    muted->SetBool("muted", false);
    object->SetDictionary("mutedInfo", muted);
    if (SeesContents(extension, tab)) {
      object->SetString("url", tab.url);
      object->SetString("title", tab.title);
      if (!tab.favicon.empty()) object->SetString("favIconUrl", tab.favicon);
    }
    return object;
  }
  CefRefPtr<CefDictionaryValue> WindowObject(const std::string& extension, const ExtensionTab& tab, bool populate) const {
    auto object = CefDictionaryValue::Create();
    object->SetInt("id", tab.id);
    object->SetBool("focused", tab.tab == hooks_.focused());
    object->SetBool("alwaysOnTop", false);
    object->SetBool("incognito", hooks_.private_mode);
    object->SetString("type", "normal");
    object->SetString("state", "normal");
    object->SetInt("left", 0);
    object->SetInt("top", 0);
    object->SetInt("width", tab.width);
    object->SetInt("height", tab.height);
    if (populate) {
      auto list = CefListValue::Create();
      list->SetDictionary(0, TabObject(extension, tab));
      object->SetList("tabs", list);
    }
    return object;
  }

  bool Matches(const std::string& extension, const ExtensionTab& tab, CefRefPtr<CefDictionaryValue> query, const std::vector<ExtensionTab>& tabs, const ExtensionTab* from) const {
    auto flag = [&](const char* key) -> std::optional<bool> {
      if (query->GetType(key) != VTYPE_BOOL) return std::nullopt;
      return query->GetBool(key);
    };
    const int focused = hooks_.focused();
    // Every tab is the only one, and so the active one, of its window.
    for (const char* key : {"active", "highlighted"}) if (flag(key) == false) return false;
    if (flag("pinned") == true || flag("audible") == true || flag("muted") == true || flag("discarded") == true) return false;
    if (auto current = flag("currentWindow")) {
      const auto* mine = Current(tabs, from);
      if ((mine && mine->tab == tab.tab) != *current) return false;
    }
    if (auto last = flag("lastFocusedWindow")) if ((tab.tab == focused) != *last) return false;
    if (auto window = IntOf(query, "windowId")) {
      // WINDOW_ID_CURRENT
      if (*window == -2) { const auto* mine = Current(tabs, from); if (!mine || mine->tab != tab.tab) return false; }
      else if (*window != tab.id) return false;
    }
    if (auto index = IntOf(query, "index")) if (*index != 0) return false;
    const auto status = StringOf(query, "status");
    if (!status.empty() && status != (tab.loading ? "loading" : "complete")) return false;
    const bool sees = SeesContents(extension, tab);
    const auto title = StringOf(query, "title");
    if (!title.empty() && (!sees || !GlobMatch(title, tab.title))) return false;
    if (query->HasKey("url")) {
      std::vector<std::string> patterns;
      if (query->GetType("url") == VTYPE_STRING) patterns.push_back(query->GetString("url"));
      else if (query->GetType("url") == VTYPE_LIST) {
        auto list = query->GetList("url");
        for (size_t i = 0; i < list->GetSize(); ++i) if (list->GetType(i) == VTYPE_STRING) patterns.push_back(list->GetString(i));
      }
      bool any = false;
      for (const auto& pattern : patterns) any = any || MatchPattern(pattern, tab.url);
      if (!sees || !any) return false;
    }
    return true;
  }

  void Create(const std::string& extension, int opener, const std::string& url, bool background, bool window, Reply reply) {
    if (!Navigable(extension, url)) { Fail(reply, "Cannot open " + url); return; }
    creating_.push_back(Creating{extension, url, window, reply});
    hooks_.open(opener, url, background);
    // The Browser opens the tab in a window of its own, in its own time;
    // an extension waits for it as long as a person would.
    const uint64_t creation = ++creation_;
    creations_[creation] = url;
    CefPostDelayedTask(TID_UI, base::BindOnce([](ExtensionTabs* self, uint64_t creation) { self->CreateTimedOut(creation); }, this, creation), 10000);
  }
  void CreateTimedOut(uint64_t creation) {
    auto url = creations_.find(creation);
    if (url == creations_.end()) return;
    for (auto it = creating_.begin(); it != creating_.end(); ++it) {
      if (it->url != url->second) continue;
      auto pending = std::move(*it);
      creating_.erase(it);
      Fail(pending.reply, "The new tab did not open");
      break;
    }
    creations_.erase(url);
  }

  // The extension's isolated world in the tab's main frame, where its
  // content scripts run: 0 when it has none there. `then` runs with the
  // world, the main frame and whether the inspector must stop reporting.
  void World(const std::string& extension, const ExtensionTab& tab, std::function<void(int, std::string)> then) {
    auto inspector = hooks_.inspector(tab.tab);
    if (!inspector) { then(0, ""); return; }
    const bool keep = hooks_.keeps_contexts(tab.tab);
    inspector->Call("Page.getFrameTree", nullptr, [this, inspector, extension, keep, then](bool ok, CefRefPtr<CefDictionaryValue> result) {
      auto tree = ok && result ? result->GetDictionary("frameTree") : nullptr;
      const std::string frame = tree ? StringOf(tree->GetDictionary("frame"), "id") : std::string();
      if (frame.empty()) { then(0, ""); return; }
      inspector->Collect(true);
      inspector->Call("Runtime.enable", nullptr, [this, inspector, extension, keep, frame, then](bool, CefRefPtr<CefDictionaryValue>) {
        int world = 0;
        for (const auto& context : inspector->contexts()) {
          auto aux = context->GetDictionary("auxData");
          if (StringOf(context, "origin") == "chrome-extension://" + extension && aux && StringOf(aux, "type") == "isolated" && StringOf(aux, "frameId") == frame)
            world = IntOf(context, "id").value_or(0);
        }
        inspector->Collect(false);
        if (!keep) inspector->Call("Runtime.disable", nullptr, [](bool, CefRefPtr<CefDictionaryValue>) {});
        then(world, frame);
      });
    });
  }
  // Run `expression` in `context` (the page's own world when 0) and answer
  // with its value.
  static void Evaluate(CefRefPtr<TabInspector> inspector, int context, const std::string& expression, std::function<void(std::optional<std::string>, CefRefPtr<CefValue>)> then) {
    auto params = CefDictionaryValue::Create();
    params->SetString("expression", expression);
    if (context) params->SetInt("contextId", context);
    params->SetBool("awaitPromise", true);
    params->SetBool("returnByValue", true);
    params->SetBool("userGesture", true);
    inspector->Call("Runtime.evaluate", params, [then](bool ok, CefRefPtr<CefDictionaryValue> result) {
      if (!ok || !result) { then(std::string("The page could not run the script"), nullptr); return; }
      if (auto thrown = result->GetDictionary("exceptionDetails")) {
        auto exception = thrown->GetDictionary("exception");
        auto message = StringOf(exception, "description");
        then(message.empty() ? StringOf(thrown, "text") : message, nullptr);
        return;
      }
      auto remote = result->GetDictionary("result");
      then(std::nullopt, remote && remote->HasKey("value") ? remote->GetValue("value")->Copy() : nullptr);
    });
  }
  // A world for an extension that has none in the page yet (it was given the
  // page by a click): the page's document, apart from the page's own scripts.
  static void FallbackWorld(CefRefPtr<TabInspector> inspector, const std::string& frame, const std::string& extension, std::function<void(int)> then) {
    auto params = CefDictionaryValue::Create();
    params->SetString("frameId", frame);
    params->SetString("worldName", "tilo-extension-" + extension);
    params->SetBool("grantUniveralAccess", false);
    inspector->Call("Page.createIsolatedWorld", params, [then](bool ok, CefRefPtr<CefDictionaryValue> result) {
      then(ok && result ? IntOf(result, "executionContextId").value_or(0) : 0);
    });
  }

  void ExecuteScript(const std::string& extension, const ExtensionTab& tab, CefRefPtr<CefDictionaryValue> args, Reply reply) {
    std::vector<std::string> sources;
    const auto func = StringOf(args, "func");
    if (!func.empty()) {
      auto call_args = args->GetType("args") == VTYPE_LIST ? args->GetList("args") : CefListValue::Create();
      sources.push_back("(" + func + ")(..." + ToJson(Wrap(call_args)) + ")");
    }
    if (auto files = args->GetList("files")) for (size_t i = 0; i < files->GetSize(); ++i) sources.push_back(files->GetString(i));
    if (sources.empty()) { Fail(reply, "Either 'func' or 'files' must be specified"); return; }
    const bool main_world = StringOf(args, "world") == "MAIN";
    auto content = args->GetType("content") == VTYPE_DICTIONARY ? args->GetDictionary("content")->Copy(false) : CefDictionaryValue::Create();
    auto inspector = hooks_.inspector(tab.tab);
    if (!inspector) { Fail(reply, "No tab with id: " + std::to_string(tab.id)); return; }
    const int key = tab.tab;
    World(extension, tab, [this, inspector, extension, key, content, sources, main_world, reply](int world, std::string frame) {
      auto run = [inspector, sources, reply](int context) {
        auto results = std::make_shared<CefRefPtr<CefValue>>();
        auto next = std::make_shared<std::function<void(size_t)>>();
        *next = [inspector, sources, reply, context, results, next](size_t index) {
          if (index == sources.size()) {
            auto entry = CefDictionaryValue::Create();
            entry->SetInt("frameId", 0);
            if (*results) entry->SetValue("result", *results); else entry->SetNull("result");
            auto list = CefListValue::Create();
            list->SetDictionary(0, entry);
            Succeed(reply, Wrap(list));
            *next = nullptr;
            return;
          }
          Evaluate(inspector, context, sources[index], [reply, results, next, index](std::optional<std::string> error, CefRefPtr<CefValue> value) {
            if (error) { Fail(reply, *error); *next = nullptr; return; }
            *results = value;
            (*next)(index + 1);
          });
        };
        (*next)(0);
      };
      if (main_world) { run(0); return; }
      if (world) { run(world); return; }
      if (frame.empty()) { Fail(reply, "The page could not run the script"); return; }
      ContentWorld(extension, key, inspector, frame, content, [run, reply](int context) {
        if (!context) { Fail(reply, "The page could not run the script"); return; }
        run(context);
      });
    });
  }
  // The world the host made for the extension in the tab's page, where its
  // scripts get a content script's `chrome` (extension_content.js): made
  // once per page, so a later script and message find the listeners an
  // earlier one added.
  void ContentWorld(const std::string& extension, int tab, CefRefPtr<TabInspector> inspector, const std::string& frame, CefRefPtr<CefDictionaryValue> content, std::function<void(int)> then) {
    auto known = content_worlds_.find({tab, extension});
    if (known != content_worlds_.end()) { then(known->second); return; }
    FallbackWorld(inspector, frame, extension, [this, extension, tab, inspector, content, then](int context) {
      if (!context) { then(0); return; }
      auto binding = CefDictionaryValue::Create();
      binding->SetString("name", "__tiloExtension");
      binding->SetString("executionContextName", "tilo-extension-" + extension);
      inspector->Call("Runtime.addBinding", binding, [this, extension, tab, inspector, content, context, then](bool, CefRefPtr<CefDictionaryValue>) {
        inspector->Listen([this, tab](CefRefPtr<CefDictionaryValue> call) { FromContent(tab, call); });
        content->SetString("id", extension);
        Evaluate(inspector, context, "(" + std::string(kExtensionContent) + ")(" + ToJson(Wrap(content)) + ")",
                 [this, extension, tab, context, then](std::optional<std::string> error, CefRefPtr<CefValue>) {
          if (error) { then(0); return; }
          content_worlds_[{tab, extension}] = context;
          then(context);
        });
      });
    });
  }
  // A call from a world the host made: a message for the extension, or its
  // storage.
  void FromContent(int tab, CefRefPtr<CefDictionaryValue> call) {
    if (StringOf(call, "name") != "__tiloExtension") return;
    const int context = IntOf(call, "executionContextId").value_or(0);
    std::string extension;
    for (const auto& [key, world] : content_worlds_) if (key.first == tab && world == context) extension = key.second;
    auto inspector = hooks_.inspector(tab);
    auto parsed = CefParseJSON(StringOf(call, "payload"), JSON_PARSER_RFC);
    auto request = parsed && parsed->GetType() == VTYPE_DICTIONARY ? parsed->GetDictionary() : nullptr;
    if (extension.empty() || !inspector || !request) return;
    const int id = IntOf(request, "id").value_or(0);
    auto settle = [inspector, context, id](bool ok, CefRefPtr<CefValue> value) {
      auto result = value ? value : CefValue::Create();
      Evaluate(inspector, context, "__tiloExtensionContent.settle(" + std::to_string(id) + ", " + (ok ? "true" : "false") + ", " + ToJson(result) + ")",
               [](std::optional<std::string>, CefRefPtr<CefValue>) {});
    };
    auto failure = [settle](const std::string& message) {
      auto text = CefValue::Create();
      text->SetString(message);
      settle(false, text);
    };
    auto args = request->GetDictionary("args");
    const auto method = StringOf(request, "method");
    if (method == "sendMessage" && args) {
      const auto tabs = Tabs();
      const auto* from = Find(tabs, tab);
      if (!from) return;
      auto sender = CefDictionaryValue::Create();
      sender->SetString("id", extension);
      sender->SetString("url", from->url);
      sender->SetInt("frameId", 0);
      sender->SetDictionary("tab", TabObject(extension, *from));
      InWorker(extension, Dispatch(args->HasKey("message") ? args->GetValue("message") : nullptr, sender), false,
               [settle, failure](std::optional<std::string> error, CefRefPtr<CefValue> value) {
        if (error) { failure(*error); return; }
        auto answer = value && value->GetType() == VTYPE_DICTIONARY ? value->GetDictionary() : nullptr;
        settle(true, answer && answer->HasKey("reply") ? answer->GetValue("reply")->Copy() : nullptr);
      });
    } else if (method == "storage" && args) {
      static const std::set<std::string> areas{"local", "sync", "session"};
      static const std::set<std::string> ops{"get", "set", "remove", "clear", "getBytesInUse"};
      const auto area = StringOf(args, "area");
      const auto op = StringOf(args, "op");
      auto values = args->GetType("args") == VTYPE_LIST ? args->GetList("args") : CefListValue::Create();
      if (!areas.count(area) || !ops.count(op)) { failure("Unknown storage call"); return; }
      InWorker(extension, "chrome.storage." + area + "." + op + "(..." + ToJson(Wrap(values)) + ")", false,
               [settle, failure](std::optional<std::string> error, CefRefPtr<CefValue> value) {
        if (error) failure(*error); else settle(true, value);
      });
    } else {
      failure("Unknown call");
    }
  }
  // Deliver a message to `chrome.runtime.onMessage` where the expression runs,
  // answered as Chrome answers it: by the first reply, or with nothing.
  static std::string Dispatch(CefRefPtr<CefValue> message, CefRefPtr<CefDictionaryValue> sender) {
    return "new Promise((resolve) => {"
           "  let replied = false;"
           "  const send = (value) => { if (!replied) { replied = true; resolve({reply: value === undefined ? null : value}); } };"
           "  const out = chrome.runtime.onMessage.dispatch(" + ToJson(message ? message->Copy() : CefValue::Create()) + ", " + ToJson(Wrap(sender)) + ", send);"
           "  if (replied) return;"
           "  const results = (out && out.results) || [];"
           "  let waits = false;"
           "  for (const result of results) {"
           "    if (result === true) waits = true;"
           "    else if (result && typeof result.then === 'function') { waits = true; result.then(send, () => send(undefined)); }"
           "  }"
           "  if (!waits) resolve({});"
           "})";
  }
  void InsertCss(const std::string& extension, const ExtensionTab& tab, const std::string& css, Reply reply) {
    auto inspector = hooks_.inspector(tab.tab);
    if (!inspector) { Fail(reply, "No tab with id: " + std::to_string(tab.id)); return; }
    auto text = CefValue::Create();
    text->SetString(css);
    const std::string expression = "(() => { const style = document.createElement('style'); style.textContent = " + ToJson(text) +
        "; (document.head || document.documentElement).appendChild(style); })()";
    World(extension, tab, [inspector, extension, expression, reply](int world, std::string frame) {
      auto run = [inspector, expression, reply](int context) {
        Evaluate(inspector, context, expression, [reply](std::optional<std::string> error, CefRefPtr<CefValue>) {
          if (error) Fail(reply, *error); else Succeed(reply, nullptr);
        });
      };
      if (world) { run(world); return; }
      if (frame.empty()) { Fail(reply, "The page could not take the style"); return; }
      FallbackWorld(inspector, frame, extension, [run, reply](int context) {
        if (!context) { Fail(reply, "The page could not take the style"); return; }
        run(context);
      });
    });
  }
  // A message for the extension's content scripts in the tab, answered as
  // Chrome answers it: by the first reply, or with nothing.
  void SendMessage(const std::string& extension, const ExtensionTab& tab, CefRefPtr<CefValue> reference, Reply reply) {
    // A value read from the request is valid only while the request is.
    auto message = reference ? reference->Copy() : CefValue::Create();
    auto inspector = hooks_.inspector(tab.tab);
    if (!inspector) { Fail(reply, "Could not establish connection. Receiving end does not exist."); return; }
    auto sender = CefDictionaryValue::Create();
    sender->SetString("id", extension);
    sender->SetString("origin", "chrome-extension://" + extension);
    auto answer = [reply](std::optional<std::string> error, CefRefPtr<CefValue> value) {
      if (error) { Fail(reply, *error); return; }
      auto result = value && value->GetType() == VTYPE_DICTIONARY ? value->GetDictionary() : nullptr;
      Succeed(reply, result && result->HasKey("reply") ? result->GetValue("reply")->Copy() : nullptr);
    };
    const std::string expression = Dispatch(message, sender);
    const int key = tab.tab;
    World(extension, tab, [this, inspector, extension, key, message, sender, expression, answer, reply](int world, std::string) {
      if (world) { Evaluate(inspector, world, expression, answer); return; }
      // Its script injected on demand listens in the world the host made.
      auto made = content_worlds_.find({key, extension});
      if (made == content_worlds_.end()) { Fail(reply, "Could not establish connection. Receiving end does not exist."); return; }
      Evaluate(inspector, made->second, "__tiloExtensionContent.deliver(" + ToJson(message) + ", " + ToJson(Wrap(sender)) + ")", answer);
    });
  }

  // Tell the extension's service worker of an event: now if it runs, after
  // waking it if it sleeps. A click reaches it whether or not it said it
  // listens, as Chrome starts a worker for one.
  void Push(const std::string& extension, const std::string& event, CefRefPtr<CefListValue> args, bool always) {
    if (!always && !(listening_.count(extension) && listening_[extension].count(event))) return;
    auto name = CefValue::Create();
    name->SetString(event);
    InWorker(extension, "globalThis.__tiloExtensionTabs ? __tiloExtensionTabs.dispatch(" + ToJson(name) + ", " + ToJson(Wrap(args)) + ") : -1",
             event == "action.onClicked", [](std::optional<std::string>, CefRefPtr<CefValue>) {});
  }
  // Evaluate `expression` in the extension's service worker, waking it if
  // it sleeps, and answer with its value (a promise's, once it settles).
  void InWorker(const std::string& extension, const std::string& expression, bool gesture, Answer answer) {
    auto& queue = waiting_[extension];
    if (queue.size() >= 64) {
      auto dropped = std::move(queue.front());
      queue.erase(queue.begin());
      dropped.answer(std::string("The extension is busy"), nullptr);
    }
    queue.push_back(Waiting{expression, gesture, std::move(answer)});
    if (queue.size() > 1) return;
    // A worker that never comes (it failed to start) does not hold back what
    // waits after it.
    const uint64_t generation = ++generation_;
    generations_[extension] = generation;
    CefPostDelayedTask(TID_UI, base::BindOnce([](ExtensionTabs* self, std::string extension, uint64_t generation) {
      auto found = self->generations_.find(extension);
      if (found != self->generations_.end() && found->second == generation) { self->generations_.erase(found); self->Abandon(extension); }
    }, this, extension, generation), 15000);
    Attach(extension, true);
  }
  // Attach to the extension's running worker and give it what waits for it;
  // with none running, wake it once and look again.
  // `looks` is how often to look again for a worker just woken, which
  // Chromium lists a moment after it says it started it.
  void Attach(const std::string& extension, bool wake, int looks = 0) {
    hooks_.pipe("Target.getTargets", nullptr, "", [this, extension, wake, looks](bool ok, CefRefPtr<CefDictionaryValue> result) {
      const std::string prefix = "chrome-extension://" + extension + "/";
      std::string target;
      auto infos = ok && result ? result->GetList("targetInfos") : nullptr;
      for (size_t i = 0; infos && i < infos->GetSize(); ++i) {
        auto info = infos->GetDictionary(i);
        if (StringOf(info, "type") == "service_worker" && StringOf(info, "url").rfind(prefix, 0) == 0) target = StringOf(info, "targetId");
      }
      if (target.empty()) {
        if (wake) Wake(extension);
        else if (looks > 0) {
          CefPostDelayedTask(TID_UI, base::BindOnce([](ExtensionTabs* self, std::string extension, int looks) {
            self->Attach(extension, false, looks - 1);
          }, this, extension, looks), 50);
        } else Abandon(extension);
        return;
      }
      auto params = CefDictionaryValue::Create();
      params->SetString("targetId", target);
      params->SetBool("flatten", true);
      hooks_.pipe("Target.attachToTarget", params, "", [this, extension](bool ok, CefRefPtr<CefDictionaryValue> result) {
        const auto session = ok && result ? StringOf(result, "sessionId") : std::string();
        if (session.empty()) { Abandon(extension); return; }
        Deliver(extension, session);
      });
    });
  }
  void Abandon(const std::string& extension) {
    auto queue = std::move(waiting_[extension]);
    waiting_.erase(extension);
    for (auto& waiting : queue) waiting.answer(std::string("The extension did not start"), nullptr);
  }
  // Start a sleeping worker, then find it.
  void Wake(const std::string& extension) {
    const auto tabs = Tabs();
    if (tabs.empty()) { Abandon(extension); return; }
    auto inspector = hooks_.inspector(tabs.front().tab);
    if (!inspector) { Abandon(extension); return; }
    inspector->Call("ServiceWorker.enable", nullptr, [this, inspector, extension](bool, CefRefPtr<CefDictionaryValue>) {
      auto params = CefDictionaryValue::Create();
      params->SetString("scopeURL", "chrome-extension://" + extension + "/");
      inspector->Call("ServiceWorker.startWorker", params, [this, inspector, extension](bool ok, CefRefPtr<CefDictionaryValue>) {
        inspector->Call("ServiceWorker.disable", nullptr, [](bool, CefRefPtr<CefDictionaryValue>) {});
        if (ok) Attach(extension, false, 40); else Abandon(extension);
      });
    });
  }
  void Deliver(const std::string& extension, const std::string& session) {
    auto queue = std::make_shared<std::vector<Waiting>>(std::move(waiting_[extension]));
    waiting_.erase(extension);
    generations_.erase(extension);
    auto detach = [this, session] {
      hooks_.pipe("Target.detachFromTarget", Session(session), "", [](bool, CefRefPtr<CefDictionaryValue>) {});
    };
    if (queue->empty()) { detach(); return; }
    Ready(session, 0, [this, queue, session, detach] {
      auto remaining = std::make_shared<size_t>(queue->size());
      for (auto& waiting : *queue) Run(session, std::move(waiting), [remaining, detach] { if (--*remaining == 0) detach(); });
    });
  }
  // Wait until the worker ran its first script, which registers its
  // listeners: a woken worker is attached to as it starts. One the Browser
  // could not give the tab APIs is waited for no longer than a moment.
  void Ready(const std::string& session, int tries, std::function<void()> then) {
    auto params = CefDictionaryValue::Create();
    params->SetString("expression", "!!(globalThis.__tiloExtensionTabs && __tiloExtensionTabs.ready)");
    params->SetBool("returnByValue", true);
    hooks_.pipe("Runtime.evaluate", params, session, [this, session, tries, then](bool ok, CefRefPtr<CefDictionaryValue> result) {
      auto remote = ok && result ? result->GetDictionary("result") : nullptr;
      if ((remote && remote->GetType("value") == VTYPE_BOOL && remote->GetBool("value")) || tries >= 40) { then(); return; }
      CefPostDelayedTask(TID_UI, base::BindOnce([](ExtensionTabs* self, std::string session, int tries, std::function<void()> then) {
        self->Ready(session, tries + 1, then);
      }, this, session, tries, then), 25);
    });
  }
  void Run(const std::string& session, Waiting waiting, std::function<void()> done) {
    auto params = CefDictionaryValue::Create();
    params->SetString("expression", waiting.expression);
    params->SetBool("silent", true);
    params->SetBool("awaitPromise", true);
    params->SetBool("returnByValue", true);
    params->SetBool("userGesture", waiting.gesture);
    hooks_.pipe("Runtime.evaluate", params, session, [done, answer = std::move(waiting.answer)](bool ok, CefRefPtr<CefDictionaryValue> result) {
      if (!ok || !result) answer(std::string("The extension could not answer"), nullptr);
      else if (auto thrown = result->GetDictionary("exceptionDetails")) {
        auto message = StringOf(thrown->GetDictionary("exception"), "description");
        answer(message.empty() ? StringOf(thrown, "text") : message, nullptr);
      } else {
        auto remote = result->GetDictionary("result");
        answer(std::nullopt, remote && remote->HasKey("value") ? remote->GetValue("value")->Copy() : nullptr);
      }
      done();
    });
  }

  Hooks hooks_;
  std::map<std::string, Access> access_;
  std::map<std::string, std::set<std::string>> listening_;
  std::set<std::pair<std::string, int>> granted_;
  std::map<std::string, std::vector<Waiting>> waiting_;
  std::vector<Creating> creating_;
  std::vector<Held> held_;
  std::map<uint64_t, std::string> creations_;
  uint64_t creation_ = 0;
  std::map<std::string, uint64_t> generations_;
  // Each worker token, to the extension whose it is.
  std::map<std::string, std::string> tokens_;
  std::string identity_;
  // Each tab's id among Chromium's, once learnt; it lasts the tab's life.
  std::map<int, int> native_;
  std::map<int, std::vector<std::function<void()>>> identifying_;
  // The worlds the host made for extensions' scripts, by tab and extension.
  std::map<std::pair<int, std::string>, int> content_worlds_;
  // When each extension last captured a tab, within the last second.
  std::map<std::string, std::deque<std::chrono::steady_clock::time_point>> captures_;
  uint64_t generation_ = 0;
};

// A request to the bridge, read here and answered on the UI thread.
class ExtensionBridgeResource final : public CefResourceHandler {
 public:
  using Start = std::function<void(int browser, const std::string& frame, const std::string& method, const std::string& body, ExtensionTabs::Reply reply)>;
  ExtensionBridgeResource(int browser, std::string frame, Start start)
      : browser_(browser), frame_(std::move(frame)), start_(std::move(start)) {}
  bool Open(CefRefPtr<CefRequest> request, bool& handle_request, CefRefPtr<CefCallback> callback) override {
    handle_request = false;
    std::string body;
    if (auto post = request->GetPostData()) {
      CefPostData::ElementVector elements;
      post->GetElements(elements);
      for (const auto& element : elements) {
        if (element->GetType() != PDE_TYPE_BYTES) continue;
        const size_t size = element->GetBytesCount();
        if (body.size() + size > kMaxBridgeRequest) { Answer(R"({"error":"The request is too large"})"); handle_request = true; return true; }
        std::string chunk(size, '\0');
        element->GetBytes(size, chunk.data());
        body += chunk;
      }
    }
    const std::string url = request->GetURL().ToString();
    const std::string method = url.size() > sizeof(kExtensionBridge) - 1 ? url.substr(sizeof(kExtensionBridge) - 1) : std::string();
    CefRefPtr<ExtensionBridgeResource> self(this);
    CefPostTask(TID_UI, base::BindOnce([](CefRefPtr<ExtensionBridgeResource> self, std::string method, std::string body, CefRefPtr<CefCallback> callback) {
      self->start_(self->browser_, self->frame_, method, body, [self, callback](const std::string& reply) {
        self->Answer(reply);
        callback->Continue();
      });
    }, self, method, std::move(body), callback));
    return true;
  }
  void GetResponseHeaders(CefRefPtr<CefResponse> response, int64_t& length, CefString&) override {
    std::lock_guard<std::mutex> lock(mutex_);
    response->SetStatus(200);
    response->SetStatusText("OK");
    response->SetMimeType("application/json");
    CefResponse::HeaderMap headers;
    // Only an extension is answered at all (ExtensionTabs::Request).
    headers.insert({"Access-Control-Allow-Origin", "*"});
    headers.insert({"Cache-Control", "no-store"});
    response->SetHeaderMap(headers);
    length = static_cast<int64_t>(body_.size());
  }
  bool Read(void* out, int want, int& read, CefRefPtr<CefResourceReadCallback>) override {
    std::lock_guard<std::mutex> lock(mutex_);
    const size_t left = body_.size() - offset_;
    read = static_cast<int>(std::min<size_t>(left, static_cast<size_t>(want)));
    if (read <= 0) { read = 0; return false; }
    std::memcpy(out, body_.data() + offset_, static_cast<size_t>(read));
    offset_ += static_cast<size_t>(read);
    return true;
  }
  void Cancel() override {}

 private:
  void Answer(const std::string& reply) { std::lock_guard<std::mutex> lock(mutex_); body_ = reply; }
  int browser_;
  std::string frame_;
  Start start_;
  std::mutex mutex_;
  std::string body_;
  size_t offset_ = 0;
  IMPLEMENT_REFCOUNTING(ExtensionBridgeResource);
};
// Serves the bridge's address in a request context: an extension's worker
// runs in the profile's own context, its pages in the host's.
class ExtensionBridgeFactory final : public CefSchemeHandlerFactory {
 public:
  explicit ExtensionBridgeFactory(ExtensionBridgeResource::Start start) : start_(std::move(start)) {}
  CefRefPtr<CefResourceHandler> Create(CefRefPtr<CefBrowser> browser, CefRefPtr<CefFrame> frame, const CefString&, CefRefPtr<CefRequest> request) override {
    if (request->GetURL().ToString().rfind(kExtensionBridge, 0) != 0 || request->GetMethod() != "POST") return nullptr;
    // Who asks is settled on the UI thread (ExtensionTabs::Request).
    return new ExtensionBridgeResource(browser ? browser->GetIdentifier() : 0, frame ? frame->GetURL().ToString() : std::string(), start_);
  }
 private:
  ExtensionBridgeResource::Start start_;
  IMPLEMENT_REFCOUNTING(ExtensionBridgeFactory);
};

}  // namespace tilo
