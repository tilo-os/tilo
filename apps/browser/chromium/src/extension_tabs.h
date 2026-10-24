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
// the host last painted of it, and runs extensions' keyboard shortcuts
// (extension_commands.h) on the keys the Browser forwards into a page. Events
// (a click, a command, a tab loading)
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
#include "extension_commands.h"
#include "extension_menus.h"
#include "extension_tabs_script.h"
#include "match_pattern.h"

namespace tilo {

inline constexpr char kExtensionBridge[] = "https://extension-bridge.tilo.invalid/";
inline constexpr size_t kMaxBridgeRequest = 8 * 1024 * 1024;
// The most frames of one page an injection reaches.
inline constexpr size_t kMaxFrames = 256;
// What Chrome answers a message or a port nothing receives.
inline constexpr char kNoReceiver[] = "Could not establish connection. Receiving end does not exist.";
// What the extension's world in a frame evaluates to learn which frame it
// is, as Chromium tells the extension (`sender.frameId`,
// `sender.documentId`): a port to the extension that only the tab script
// answers (extension_tabs.js). A port and not a message, which an extension
// page without the tab script (its UI in a page's frame) may answer first:
// a port carries every end's answer.
inline constexpr char kFrameProbe[] = R"JS(new Promise((resolve) => {
  let port;
  try { port = chrome.runtime.connect({name: '__tiloFrame'}); } catch (error) { resolve(null); return; }
  let late = 0;
  const done = (value) => {
    clearTimeout(late);
    resolve(value);
    try { port.disconnect(); } catch (error) {}
  };
  late = setTimeout(() => done(null), 5000);
  port.onMessage.addListener((told) => { if (told && Number.isInteger(told.frameId)) done(told); });
  port.onDisconnect.addListener(() => done(null));
}))JS";

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
  double zoom = 1;
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
  // An execution context a session on the pipe reports.
  using PipeListener = std::function<void(CefRefPtr<CefDictionaryValue>)>;
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
    // Hear the contexts a session the host attached on the pipe reports;
    // an empty listener stops.
    std::function<void(const std::string&, PipeListener)> pipe_listen;
    // Keep the extensions' context menu items (ExtensionMenus::Save) with
    // the profile; unset for a private one.
    std::function<void(std::string)> save_menus;
    // An event for the Browser about one of the host's tabs.
    std::function<void(int, const std::string&, CefRefPtr<CefDictionaryValue>)> event;
    bool private_mode = false;
  };
  explicit ExtensionTabs(Hooks hooks) : hooks_(std::move(hooks)) {}
  // A change not yet kept is kept as the host closes.
  ~ExtensionTabs() { if (saving_ && hooks_.save_menus) hooks_.save_menus(menus_.Save()); }

  // The Browser's own extension, whose content script is in every page and
  // whose worker answers which tab a page is (Identify).
  void SetIdentity(const std::string& id) { identity_ = id; }

  // What the extension may do, from its manifest as installed: its API
  // permissions, its host permissions (`hosts`) and, apart from those, where
  // its declared content scripts run (`scriptable`), which as in Chrome
  // gives no access to the tab APIs.
  // `token` is what its worker shows the bridge: the Browser wrote it with
  // the tab APIs into the extension's worker, which alone can read it.
  // `commands` are its manifest's (`commands`), keyboard shortcuts.
  void SetAccess(const std::string& id, std::set<std::string> permissions, std::vector<std::string> hosts, std::vector<std::string> scriptable,
                 const std::string& token, CefRefPtr<CefDictionaryValue> commands) {
    access_[id] = Access{std::move(permissions), std::move(hosts), std::move(scriptable)};
    commands_.Set(id, CommandsOf(commands));
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
    commands_.Forget(id);
    for (auto it = tokens_.begin(); it != tokens_.end();) it = it->second == id ? tokens_.erase(it) : std::next(it);
    listening_.erase(id);
    for (auto it = granted_.begin(); it != granted_.end();) it = it->first == id ? granted_.erase(it) : std::next(it);
    captures_.erase(id);
    for (auto it = ports_.begin(); it != ports_.end();) it = it->second.extension == id ? ports_.erase(it) : std::next(it);
    // Removed: its menu items go with it, as in Chrome.
    menus_.RemoveAll(id);
    MenusChanged();
    for (auto it = action_states_.begin(); it != action_states_.end();) it = it->first.first == id ? action_states_.erase(it) : std::next(it);
    // Chromium starts it again with its button as the manifest made it.
    buttons_.erase(id);
  }
  // The items the profile kept from an earlier run.
  void LoadMenus(const std::string& json) { menus_.Load(json); }

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
      if (!tab) { Fail(reply, kNoReceiver); return; }
      SendMessage(extension, *tab, args->GetValue("message"), IntOf(args, "frameId"), StringOf(args, "documentId"), reply);
    } else if (method == "scripting.executeScript" || method == "scripting.insertCSS") {
      auto target = args->GetDictionary("target");
      const auto* tab = ByExtensionId(tabs, IntOf(target, "tabId"));
      if (!tab) { Fail(reply, "No tab with id: " + std::to_string(IntOf(target, "tabId").value_or(0))); return; }
      if (!MayScript(extension, *tab)) { Fail(reply, "Cannot access contents of the page. Extension manifest must request permission to access the respective host."); return; }
      if (method == "scripting.insertCSS") InsertCss(extension, *tab, StringOf(args, "css"), reply);
      else ExecuteScript(extension, caller, *tab, args, reply);
    } else if (method == "tabs.captureVisibleTab") {
      Capture(extension, tabs, from, args, reply);
    } else if (method == "ports") {
      // What the extension's ports to scripts in pages carry, in order.
      auto ops = args->GetList("ops");
      for (size_t i = 0; ops && i < ops->GetSize(); ++i)
        if (ops->GetType(i) == VTYPE_DICTIONARY) FromEnd(extension, caller, ops->GetDictionary(i));
      Succeed(reply, nullptr);
    } else if (method == "storage.changed") {
      // The worker hears every change to the extension's storage; a page
      // tells of one only for an extension without a worker, and only its
      // first page, so each change arrives once.
      const auto pages = Pages(extension);
      const bool teller = !caller || (workerless_.count(extension) && !pages.empty() && pages.front() == caller);
      auto area = CefValue::Create();
      area->SetString(StringOf(args, "area"));
      auto changes = args->GetType("changes") == VTYPE_DICTIONARY ? Wrap(args->GetDictionary("changes")->Copy(false)) : Wrap(CefDictionaryValue::Create());
      bool any = false;
      for (const auto& [key, context] : content_worlds_) {
        if (key.second != extension) continue;
        any = true;
        auto inspector = hooks_.inspector(key.first);
        if (teller && inspector)
          Evaluate(inspector, context, "__tiloExtensionContent.changed(" + ToJson(changes) + ", " + ToJson(area) + ")", [](std::optional<std::string>, CefRefPtr<CefValue>) {});
      }
      auto wanted = CefValue::Create();
      wanted->SetBool(any);
      Succeed(reply, wanted);
    } else if (method == "windows.get" || method == "windows.getCurrent" || method == "windows.getLastFocused") {
      const ExtensionTab* tab = nullptr;
      if (method == "windows.get") tab = ByExtensionId(tabs, IntOf(args, "windowId"));
      else if (method == "windows.getCurrent") tab = Current(tabs, from);
      else tab = LastFocused(tabs);
      if (!tab && method != "windows.get") { Fail(reply, "No current window"); return; }
      if (!tab) { Fail(reply, "No window with id: " + std::to_string(IntOf(args, "windowId").value_or(-1)) + "."); return; }
      Succeed(reply, Wrap(WindowObject(extension, *tab, Populate(args))));
    } else if (method == "windows.getAll") {
      auto list = CefListValue::Create();
      for (const auto& tab : tabs) if (!tab.panel) list->SetDictionary(list->GetSize(), WindowObject(extension, tab, Populate(args)));
      Succeed(reply, Wrap(list));
    } else if (method == "action.openPopup") {
      // The extension checked that the page has a popup (`getPopup`); the
      // Browser opens it over the page as its button would, without the
      // access a click gives.
      const auto* tab = ByExtensionId(tabs, IntOf(args, "tabId"));
      const auto url = StringOf(args, "popup");
      if (!tab) { Fail(reply, "Could not find an active browser window."); return; }
      if (url.size() > 2048 || url.rfind("chrome-extension://" + extension + "/", 0) != 0) { Fail(reply, "Extension does not have a popup on the active tab."); return; }
      auto fields = CefDictionaryValue::Create();
      fields->SetString("id", extension);
      fields->SetString("url", url);
      if (hooks_.event) hooks_.event(tab->tab, "extension_open_popup", fields);
      Succeed(reply, nullptr);
    } else if (method == "windows.update") {
      const auto* tab = ByExtensionId(tabs, IntOf(args, "windowId"));
      if (!tab) { Fail(reply, "No window with id: " + std::to_string(IntOf(args, "windowId").value_or(-1)) + "."); return; }
      Succeed(reply, Wrap(WindowObject(extension, *tab, false)));
    } else if (method.rfind("contextMenus.", 0) == 0) {
      if (!Has(extension, "contextMenus")) { Fail(reply, "The extension may not add menu items"); return; }
      std::optional<std::string> error;
      if (method == "contextMenus.create") error = menus_.Create(extension, args->GetDictionary("properties"));
      else if (method == "contextMenus.update") error = menus_.Update(extension, args->GetValue("id"), args->GetDictionary("properties"));
      else if (method == "contextMenus.remove") error = menus_.Remove(extension, args->GetValue("id"));
      else if (method == "contextMenus.removeAll") menus_.RemoveAll(extension);
      else error = "Tilo does not support " + method + " yet";
      if (error) { Fail(reply, *error); return; }
      MenusChanged();
      Succeed(reply, nullptr);
    } else if (method == "tabs.detectLanguage" || method == "tabs.getZoom" || method == "tabs.getZoomSettings") {
      const auto* tab = IntOf(args, "tabId") ? ByExtensionId(tabs, IntOf(args, "tabId")) : Current(tabs, from);
      if (!tab) { Fail(reply, "No tab with id: " + std::to_string(IntOf(args, "tabId").value_or(0)) + "."); return; }
      if (method == "tabs.detectLanguage") { DetectLanguage(*tab, reply); return; }
      if (method == "tabs.getZoom") {
        auto zoom = CefValue::Create();
        zoom->SetDouble(tab->zoom);
        Succeed(reply, zoom);
        return;
      }
      // Chromium's zoom belongs to the site (host.cc), in Chrome's terms.
      auto settings = CefDictionaryValue::Create();
      settings->SetString("mode", "automatic");
      settings->SetString("scope", "per-origin");
      settings->SetDouble("defaultZoomFactor", 1);
      Succeed(reply, Wrap(settings));
    } else if (method == "action.set" || method == "action.get") {
      Button(extension, tabs, method == "action.get", args, reply);
    } else if (method == "commands.getAll") {
      auto list = CefListValue::Create();
      for (const auto& command : commands_.All(extension)) {
        auto entry = CefDictionaryValue::Create();
        entry->SetString("name", command.name);
        entry->SetString("description", command.description);
        entry->SetString("shortcut", command.shortcut);
        list->SetDictionary(list->GetSize(), entry);
      }
      Succeed(reply, Wrap(list));
    } else {
      Fail(reply, "Tilo does not support " + method + " yet");
    }
  }

  // The extensions' items for the menu the person opened on a page. A web
  // page shows every extension's; an extension's own page only its
  // own; the Browser's pages none.
  std::vector<MenuOffer> MenuOffers(const MenuContext& context) const {
    std::vector<MenuOffer> offers;
    const std::string prefix = "chrome-extension://";
    const std::string& page = context.page;
    const bool web = page.rfind("http://", 0) == 0 || page.rfind("https://", 0) == 0 || page.rfind("file://", 0) == 0;
    const std::string own = page.rfind(prefix, 0) == 0 ? page.substr(prefix.size(), 32) : std::string();
    if (!web && own.empty()) return offers;
    for (const auto& [extension, access] : access_) {
      if (extension == identity_ || !access.permissions.count("contextMenus") || (!web && extension != own)) continue;
      for (auto& offer : menus_.Offers(extension, context)) offers.push_back(std::move(offer));
    }
    return offers;
  }
  // The person chose an extension's item in the menu of `tab`'s page: the
  // extension hears of it with the page, which it may then script until the
  // page navigates if it asked for activeTab, as after a click on its button.
  void MenuChosen(int tab, const MenuOffer& offer, const MenuContext& context) {
    if (!access_.count(offer.extension)) return;
    auto info = menus_.Choose(offer.extension, offer.key, context);
    if (!info) return;
    MenusChanged();
    Ensure([this, tab, offer, info] {
      const auto tabs = Tabs();
      const auto* found = Find(tabs, tab);
      if (!found || !access_.count(offer.extension)) return;
      if (Has(offer.extension, "activeTab") && !found->panel) granted_.insert({offer.extension, found->id});
      auto args = CefListValue::Create();
      args->SetDictionary(0, info);
      if (!found->panel) args->SetDictionary(1, TabObject(offer.extension, *found));
      Push(offer.extension, "contextMenus.onClicked", args, true);
    });
  }

  // The extension's toolbar button opened its popup over `tab`: one that
  // asked for activeTab may now read and script it until the page
  // navigates, as after a click.
  void Grant(const std::string& extension, int tab) {
    Ensure([this, extension, tab] {
      const auto tabs = Tabs();
      if (const auto* found = Find(tabs, tab); found && Has(extension, "activeTab")) granted_.insert({extension, found->id});
    });
  }

  // The person clicked the extension's toolbar button over `tab`: like
  // Chrome, one that asked for activeTab may now read and script that page
  // until it navigates; a click gives any other nothing.
  void Clicked(const std::string& extension, int tab) {
    Ensure([this, extension, tab] { ClickedNow(extension, tab); });
  }
  void ClickedNow(const std::string& extension, int tab) {
    const auto tabs = Tabs();
    const auto* found = Find(tabs, tab);
    if (!found || !access_.count(extension)) return;
    if (Has(extension, "activeTab")) granted_.insert({extension, found->id});
    auto args = CefListValue::Create();
    args->SetDictionary(0, TabObject(extension, *found));
    Push(extension, "action.onClicked", args, true);
  }

  // The person pressed a key on the page of `tab` (no extension's panel):
  // whether it is an extension's keyboard shortcut, which the page then does
  // not get. The extension hears of its command with the page, which it may
  // then script until the page navigates if it asked for activeTab, as in
  // Chrome; the toolbar button's own command acts as a click on the button
  // does, so the Browser is asked to (`extension_execute_action`): it opens
  // the button's popup, or tells the extension of the click.
  bool Key(int tab, const Shortcut& shortcut) {
    auto found = commands_.Find(shortcut);
    if (!found || !access_.count(found->first)) return false;
    Ensure([this, tab, command = *found] { CommandNow(command.first, command.second, tab); });
    return true;
  }
  void CommandNow(const std::string& extension, const std::string& name, int tab) {
    const auto tabs = Tabs();
    const auto* found = Find(tabs, tab);
    if (!found || found->panel || !access_.count(extension)) return;
    if (ActionCommand(name)) {
      auto fields = CefDictionaryValue::Create();
      fields->SetString("id", extension);
      if (hooks_.event) hooks_.event(tab, "extension_execute_action", fields);
      return;
    }
    if (Has(extension, "activeTab")) granted_.insert({extension, found->id});
    auto args = CefListValue::Create();
    args->SetString(0, name);
    args->SetDictionary(1, TabObject(extension, *found));
    Push(extension, "commands.onCommand", args, true);
  }

  // The tab can be told of things: what extensions set for their buttons on
  // every tab before it opened.
  void Introduce(int tab) {
    for (const auto& [extension, values] : buttons_) {
      if (values->GetSize()) TellButton(tab, extension, true, values);
    }
  }

  // A tab's page changed: `status`, `url` or `title`.
  void Updated(int tab, CefRefPtr<CefDictionaryValue> change) {
    if (change->HasKey("url")) {
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
  // The tab's main frame committed a new document: the worlds the host made
  // in the old one went with it, and so did the ports to them and from it.
  // What extensions set for their buttons on this tab alone goes too, as in
  // Chrome, and the Browser is told.
  void Navigated(int tab) {
    Drop(tab);
    const auto known = native_.find(tab);
    if (known != native_.end())
      for (auto it = action_states_.begin(); it != action_states_.end();) it = it->first.second == known->second ? action_states_.erase(it) : std::next(it);
    if (buttons_on_.erase(tab) && hooks_.event) hooks_.event(tab, "extension_action_reset", CefDictionaryValue::Create());
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
    Drop(tab);
    buttons_on_.erase(tab);
    if (!found || found->panel) return;
    for (auto it = granted_.begin(); it != granted_.end();) it = it->second == found->id ? granted_.erase(it) : std::next(it);
    for (auto it = action_states_.begin(); it != action_states_.end();) it = it->first.second == found->id ? action_states_.erase(it) : std::next(it);
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
  // `scriptable` is kept apart from `hosts` so that no tab API counts it:
  // Chromium runs the declared content scripts itself.
  struct Access { std::set<std::string> permissions; std::vector<std::string> hosts; std::vector<std::string> scriptable; };
  // A manifest's `commands`, each with the key it suggests on this
  // platform: its own (`mac`, `linux`), else its `default`, or one for all.
  static std::vector<ExtensionCommand> CommandsOf(CefRefPtr<CefDictionaryValue> commands) {
    std::vector<ExtensionCommand> declared;
    CefDictionaryValue::KeyList names;
    if (!commands || !commands->GetKeys(names)) return declared;
#if defined(OS_MAC)
    const char* platform = "mac";
#else
    const char* platform = "linux";
#endif
    for (const auto& name : names) {
      if (declared.size() >= 256 || commands->GetType(name) != VTYPE_DICTIONARY) continue;
      auto command = commands->GetDictionary(name);
      std::string suggested;
      if (command->GetType("suggested_key") == VTYPE_STRING) {
        suggested = command->GetString("suggested_key").ToString();
      } else if (auto keys = command->GetDictionary("suggested_key")) {
        suggested = keys->HasKey(platform) ? StringOf(keys, platform) : StringOf(keys, "default");
      }
      declared.push_back(ExtensionCommand{name.ToString(), StringOf(command, "description"), suggested});
    }
    return declared;
  }
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
  // A port between a script in a world the host made in a page and the
  // extension. The page's end is that world, which knows the port as
  // `local`; the extension's ends are its worker (0) and its pages (their
  // tabs), which know it by the key it has in `ports_`.
  struct Port {
    std::string extension, local;
    int tab = 0, context = 0;
    std::set<int> ends;
    // Until the other side said whether it takes the port, what either end
    // sends on it waits, in order.
    bool opening = true;
    struct Early { int from; bool disconnect; CefRefPtr<CefValue> message; };
    std::vector<Early> early;
  };
  // `from` for what the page's end of a port sends.
  static constexpr int kPageEnd = -1;
  static constexpr size_t kMaxPorts = 4096;
  // One message or closing on its way to one of an extension's ends, as
  // the arguments of `__tiloExtensionTabs.port` (extension_tabs.js).
  struct Outgoing { std::string port; bool closing; std::string values; };
  struct Outbox { std::vector<Outgoing> ops; bool sending = false; };
  // At most this much script in one delivery to an end.
  static constexpr size_t kMaxPortBatch = 4 * 1024 * 1024;
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
  // Keep the items with the profile a moment after a change: an extension
  // makes its items in a burst, which is written once.
  void MenusChanged() {
    if (!hooks_.save_menus || saving_) return;
    saving_ = true;
    // The host may close first: the destructor keeps the items then, and
    // this finds the tabs gone.
    std::weak_ptr<bool> alive = alive_;
    CefPostDelayedTask(TID_UI, base::BindOnce([](ExtensionTabs* self, std::weak_ptr<bool> alive) {
      if (alive.expired() || !self->saving_) return;
      self->saving_ = false;
      self->hooks_.save_menus(self->menus_.Save());
    }, this, alive), 250);
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
    static const std::set<std::string> names{"action.onClicked", "commands.onCommand", "contextMenus.onClicked", "tabs.onCreated", "tabs.onUpdated", "tabs.onActivated", "tabs.onRemoved", "windows.onFocusChanged"};
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
    // An extension told of a tab before the host learnt its id among
    // Chromium's (a blank page, a page still loading) was given the host's;
    // the tab keeps answering to it.
    for (const auto& entry : tabs) if (entry.tab == *id && !entry.panel) return &entry;
    return nullptr;
  }
  // The window a caller means by "current": its own tab's, the page a panel
  // was opened over, or for a service worker the one used last.
  const ExtensionTab* Current(const std::vector<ExtensionTab>& tabs, const ExtensionTab* from) const {
    if (from && !from->panel) return from;
    if (from && from->panel) if (const auto* opener = Find(tabs, from->opener)) return opener;
    return LastFocused(tabs);
  }
  // The page the person used last. Chrome always has a last focused window
  // while one is open, and so does this: an extension's worker starts with
  // the engine, before the person has focused any page, and asks then.
  const ExtensionTab* LastFocused(const std::vector<ExtensionTab>& tabs) const {
    if (const auto* tab = Find(tabs, hooks_.focused()); tab && !tab->panel) return tab;
    for (const auto& tab : tabs) if (!tab.panel) return &tab;
    return nullptr;
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
  // Whether the extension's host permissions reach `url`. Where its content
  // scripts run does not count: Chrome reads a tab and scripts it by host
  // permissions alone.
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
  // Host permission for every site (`<all_urls>`, or any host of a scheme),
  // which with `activeTab` is what Chrome asks of an extension to capture a
  // tab; content scripts on every site are not.
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
    const auto* last_focused = LastFocused(tabs);
    const int focused = last_focused ? last_focused->tab : 0;
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
    NamedWorld(extension, tab, [then](int world, std::string frame, std::string) { then(world, frame); });
  }
  // The same, with the name Chromium gave the world.
  void NamedWorld(const std::string& extension, const ExtensionTab& tab, std::function<void(int, std::string, std::string)> then) {
    auto inspector = hooks_.inspector(tab.tab);
    if (!inspector) { then(0, "", ""); return; }
    const bool keep = hooks_.keeps_contexts(tab.tab);
    inspector->Call("Page.getFrameTree", nullptr, [this, inspector, extension, keep, then](bool ok, CefRefPtr<CefDictionaryValue> result) {
      auto tree = ok && result ? result->GetDictionary("frameTree") : nullptr;
      const std::string frame = tree ? StringOf(tree->GetDictionary("frame"), "id") : std::string();
      if (frame.empty()) { then(0, "", ""); return; }
      inspector->Collect(true);
      inspector->Call("Runtime.enable", nullptr, [this, inspector, extension, keep, frame, then](bool, CefRefPtr<CefDictionaryValue>) {
        int world = 0;
        std::string name;
        for (const auto& context : inspector->contexts()) {
          auto aux = context->GetDictionary("auxData");
          if (ContentScriptWorld(context, extension) && StringOf(aux, "frameId") == frame) {
            world = IntOf(context, "id").value_or(0);
            name = StringOf(context, "name");
          }
        }
        inspector->Collect(false);
        if (!keep) inspector->Call("Runtime.disable", nullptr, [](bool, CefRefPtr<CefDictionaryValue>) {});
        then(world, frame, name);
      });
    });
  }
  // Whether an inspector's execution context is the world Chromium runs the
  // extension's content scripts in. The host's own worlds (named `tilo-…`)
  // in the extension's own pages have its origin too.
  static bool ContentScriptWorld(CefRefPtr<CefDictionaryValue> context, const std::string& extension) {
    auto aux = context ? context->GetDictionary("auxData") : nullptr;
    return aux && StringOf(aux, "type") == "isolated" && StringOf(context, "origin") == "chrome-extension://" + extension &&
           StringOf(context, "name").rfind("tilo-", 0) != 0;
  }
  // Run `expression` in `context` (the page's own world when 0) and answer
  // with its value.
  // A script an extension runs counts as the person's doing (`gesture`), as
  // after the click that let it; the host's own reads do not.
  static void Evaluate(CefRefPtr<TabInspector> inspector, int context, const std::string& expression, std::function<void(std::optional<std::string>, CefRefPtr<CefValue>)> then, bool gesture = true) {
    auto params = CefDictionaryValue::Create();
    params->SetString("expression", expression);
    if (context) params->SetInt("contextId", context);
    params->SetBool("awaitPromise", true);
    params->SetBool("returnByValue", true);
    params->SetBool("userGesture", gesture);
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

  // The frames an injection or a message names: every frame
  // (`target.allFrames`), or some, by their id among Chromium's (`frameIds`,
  // `frameId`) or by their document's (`documentIds`, `documentId`). One
  // that names none, or the main frame alone by its id, names the main frame.
  struct FrameQuery {
    bool all = false;
    std::vector<int> ids;
    std::vector<std::string> documents;
    bool MainOnly() const { return !all && ids.empty() && documents.empty(); }
  };
  // What an injection's `target` names, or why Chrome would refuse it.
  static std::optional<std::string> FramesOf(CefRefPtr<CefDictionaryValue> target, FrameQuery& wanted) {
    wanted.all = target && target->GetType("allFrames") == VTYPE_BOOL && target->GetBool("allFrames");
    auto ids = target && target->GetType("frameIds") == VTYPE_LIST ? target->GetList("frameIds") : nullptr;
    for (size_t i = 0; ids && i < ids->GetSize(); ++i)
      if (auto id = IntOf(ids->GetValue(i)); id && std::find(wanted.ids.begin(), wanted.ids.end(), *id) == wanted.ids.end()) wanted.ids.push_back(*id);
    auto documents = target && target->GetType("documentIds") == VTYPE_LIST ? target->GetList("documentIds") : nullptr;
    bool malformed = false;
    for (size_t i = 0; documents && i < documents->GetSize(); ++i) {
      const auto document = documents->GetType(i) == VTYPE_STRING ? documents->GetString(i).ToString() : std::string();
      if (document.empty() || document.size() > 64 || wanted.documents.size() >= kMaxFrames) { malformed = true; continue; }
      if (std::find(wanted.documents.begin(), wanted.documents.end(), document) == wanted.documents.end()) wanted.documents.push_back(document);
    }
    const bool by_id = ids && ids->GetSize();
    const bool by_document = documents && documents->GetSize();
    if (wanted.all && by_id) return std::string("Cannot specify both 'allFrames' and 'frameIds'.");
    if (wanted.all && by_document) return std::string("Cannot specify both 'allFrames' and 'documentIds'.");
    if (by_id && by_document) return std::string("Cannot specify both 'frameIds' and 'documentIds'.");
    if (malformed) return std::string("Error at property 'documentIds': Each must be a document's id.");
    if (!wanted.all && wanted.ids.size() == 1 && wanted.ids[0] == 0) wanted.ids.clear();
    return std::nullopt;
  }

  void ExecuteScript(const std::string& extension, int caller, const ExtensionTab& tab, CefRefPtr<CefDictionaryValue> args, Reply reply) {
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
    const int tab_id = tab.id;
    FrameQuery wanted;
    if (auto refused = FramesOf(args->GetDictionary("target"), wanted)) { Fail(reply, *refused); return; }
    Frames(extension, tab, wanted, [this, inspector, extension, caller, key, tab_id, content, sources, main_world, wanted, reply](std::vector<Frame> frames) {
      std::vector<Frame> chosen;
      for (int id : wanted.ids) {
        auto found = std::find_if(frames.begin(), frames.end(), [id](const Frame& frame) { return frame.id == id; });
        if (found == frames.end()) { Release(frames); Fail(reply, "No frame with id " + std::to_string(id) + " in tab with id " + std::to_string(tab_id) + "."); return; }
        chosen.push_back(*found);
      }
      for (const auto& document : wanted.documents) {
        auto found = std::find_if(frames.begin(), frames.end(), [&](const Frame& frame) { return frame.document == document; });
        if (found == frames.end()) { Release(frames); Fail(reply, "No document with id " + document + " in tab with id " + std::to_string(tab_id) + "."); return; }
        if (std::none_of(chosen.begin(), chosen.end(), [&](const Frame& known) { return known.devtools == found->devtools; })) chosen.push_back(*found);
      }
      // Every frame the script can run in, as Chrome passes over those it
      // cannot reach: a frame whose page the extension's scripts left (it
      // went to one they do not match) keeps its id but has no world, and
      // a page of the extension's own takes no script.
      if (wanted.all)
        for (const auto& frame : frames)
          if (frame.id >= 0 && Runnable(frame, main_world) && std::none_of(chosen.begin(), chosen.end(), [&](const Frame& known) { return known.id == frame.id; })) chosen.push_back(frame);
      if (wanted.MainOnly() && !frames.empty()) chosen.push_back(frames.front());
      // Sessions of frames not asked for go now.
      std::vector<Frame> unused;
      for (const auto& frame : frames)
        if (std::none_of(chosen.begin(), chosen.end(), [&](const Frame& known) { return known.session == frame.session; })) unused.push_back(frame);
      Release(unused);
      if (chosen.empty()) { Fail(reply, "The page could not run the script"); return; }
      // Frame by frame, each in the extension's world there (or the page's
      // own for MAIN); the main frame, where it has none, in one the host
      // makes it.
      auto results = CefListValue::Create();
      auto next = std::make_shared<std::function<void(size_t)>>();
      *next = [this, inspector, extension, caller, key, content, sources, main_world, chosen, results, reply, next](size_t index) {
        if (index == chosen.size()) { Release(chosen); Succeed(reply, Wrap(results)); *next = nullptr; return; }
        const Frame frame = chosen[index];
        auto ran = [this, chosen, results, frame, reply, next, index](std::optional<std::string> error, CefRefPtr<CefValue> value) {
          if (error) { Release(chosen); Fail(reply, *error); *next = nullptr; return; }
          auto entry = CefDictionaryValue::Create();
          entry->SetInt("frameId", frame.id);
          if (!frame.document.empty()) entry->SetString("documentId", frame.document);
          if (value) entry->SetValue("result", value); else entry->SetNull("result");
          results->SetDictionary(results->GetSize(), entry);
          (*next)(index + 1);
        };
        if (!Runnable(frame, main_world)) { ran(std::string("The page could not run the script"), nullptr); return; }
        if (main_world || frame.world) { RunSources(inspector, frame.session, main_world ? frame.page : frame.world, sources, ran); return; }
        ContentWorld(extension, caller, key, inspector, frame.devtools, content, [this, inspector, sources, ran](int context) {
          if (!context) { ran(std::string("The page could not run the script"), nullptr); return; }
          RunSources(inspector, "", context, sources, ran);
        });
      };
      (*next)(0);
    });
  }
  // Run `sources` one after another in `context` (the page's own world when
  // 0), answered with the last one's value or the first error.
  void RunSources(CefRefPtr<TabInspector> inspector, const std::string& session, int context, std::vector<std::string> sources, Answer done) {
    auto next = std::make_shared<std::function<void(size_t, CefRefPtr<CefValue>)>>();
    *next = [this, inspector, session, context, sources, done, next](size_t index, CefRefPtr<CefValue> last) {
      if (index == sources.size()) { done(std::nullopt, last); *next = nullptr; return; }
      EvaluateIn(inspector, session, context, sources[index], [done, next, index](std::optional<std::string> error, CefRefPtr<CefValue> value) {
        if (error) { done(error, nullptr); *next = nullptr; return; }
        (*next)(index + 1, value);
      });
    };
    (*next)(0, nullptr);
  }

  // A frame of a tab's page as an extension knows it: its id among
  // Chromium's (0 for the main frame, -1 while unknown), the extension's
  // world in it and the page's own (0 when there is none), and the session
  // on the pipe that reaches it when another site's process renders it
  // (empty: the tab's inspector does). Its document is known to the
  // inspector by the page's world (`loaded`) and to the extension by an id
  // Chromium gives it (`sender.documentId`), once learnt. A frame that shows
  // a page of the extension's own (`own`: the extension's UI in a page, as
  // Evernote's clipper shows its panel) is the extension's world itself.
  struct Frame {
    std::string devtools;
    int id = -1;
    int world = 0;
    int page = 0;
    std::string session;
    std::string loaded;
    std::string document;
    bool own = false;
  };
  // What the host learnt of a frame: its id, which lasts as long as the
  // frame, and the id of the document it showed then.
  struct KnownFrame { int id = -1; std::string loaded; std::string document; };
  // Whether a script can run in `frame`: in the page's own world (MAIN), or
  // in the extension's world there. Only the main frame has the page's own
  // world as context 0, and only there the host makes the extension a world
  // it lacks; in any other frame, context 0 would be the main frame's. A
  // page of the extension's own takes none, as in Chrome.
  static bool Runnable(const Frame& frame, bool main_world) {
    if (frame.own) return false;
    if (frame.id == 0 && frame.session.empty()) return true;
    return main_world ? frame.page != 0 : frame.world != 0;
  }
  // Whether the frames `query` names are all among `frames`.
  static bool Reached(const std::vector<Frame>& frames, const FrameQuery& query) {
    if (query.all || (query.ids.empty() && query.documents.empty())) return false;
    return std::all_of(query.ids.begin(), query.ids.end(), [&](int id) {
             return std::any_of(frames.begin(), frames.end(), [id](const Frame& frame) { return frame.id == id; });
           }) &&
           std::all_of(query.documents.begin(), query.documents.end(), [&](const std::string& document) {
             return std::any_of(frames.begin(), frames.end(), [&](const Frame& frame) { return frame.document == document; });
           });
  }
  // The frames of the tab's page, the main frame first; for a query that
  // names only the main frame, the main frame alone. Those the page's
  // process renders the tab's inspector reaches; a frame another site's
  // process renders is a target of its own, reached on the pipe. Chromium
  // knows a frame by an id the inspector does not show; the extension's
  // world there asks (extension_tabs.js answers), and it lasts as long as
  // the frame. The caller lets go of the frames' sessions (Release). When
  // the frames named are all among those the tab's inspector reaches, no
  // other site's frame is attached to, and a frame named that the host met
  // before on a target of its own is the only one attached to: a message to
  // one frame does not cost a session for every frame of every other site
  // on the page.
  void Frames(const std::string& extension, const ExtensionTab& tab, FrameQuery query, std::function<void(std::vector<Frame>)> then) {
    auto inspector = hooks_.inspector(tab.tab);
    if (!inspector) { then({}); return; }
    const bool keep = hooks_.keeps_contexts(tab.tab);
    const int key = tab.tab;
    inspector->Call("Page.getFrameTree", nullptr, [this, inspector, extension, keep, key, query, then](bool ok, CefRefPtr<CefDictionaryValue> result) {
      const bool main_only = query.MainOnly();
      std::vector<Frame> frames;
      std::function<void(CefRefPtr<CefDictionaryValue>)> walk = [&](CefRefPtr<CefDictionaryValue> node) {
        if (!node || frames.size() >= kMaxFrames) return;
        const auto id = StringOf(node->GetDictionary("frame"), "id");
        if (id.empty()) return;
        frames.push_back(Frame{id, frames.empty() ? 0 : -1});
        auto children = main_only ? nullptr : node->GetList("childFrames");
        for (size_t i = 0; children && i < children->GetSize(); ++i) walk(children->GetDictionary(i));
      };
      walk(ok && result ? result->GetDictionary("frameTree") : nullptr);
      if (frames.empty()) { then({}); return; }
      inspector->Collect(true);
      inspector->Call("Runtime.enable", nullptr, [this, inspector, extension, keep, key, main_only, query, frames, then](bool, CefRefPtr<CefDictionaryValue>) mutable {
        for (const auto& context : inspector->contexts()) Place(frames, context, extension, "", false);
        inspector->Collect(false);
        if (!keep) inspector->Call("Runtime.disable", nullptr, [](bool, CefRefPtr<CefDictionaryValue>) {});
        auto local = std::make_shared<std::vector<Frame>>(std::move(frames));
        Identified(inspector, key, !query.documents.empty(), local, [this, extension, key, main_only, query, local, then] {
          if (main_only || Reached(*local, query)) { then(*local); return; }
          RemoteFrames(extension, key, query, local, then);
        });
      });
    });
  }
  // Put an execution context in the frame it belongs to: the page's own, or
  // the extension's world there. With `add`, a frame not among `frames`
  // joins them.
  static void Place(std::vector<Frame>& frames, CefRefPtr<CefDictionaryValue> context, const std::string& extension, const std::string& session, bool add) {
    auto aux = context ? context->GetDictionary("auxData") : nullptr;
    const auto frame = StringOf(aux, "frameId");
    if (frame.empty()) return;
    auto found = std::find_if(frames.begin(), frames.end(), [&](const Frame& entry) { return entry.devtools == frame; });
    if (found == frames.end()) {
      if (!add || frames.size() >= kMaxFrames) return;
      frames.push_back(Frame{frame, -1, 0, 0, session});
      found = std::prev(frames.end());
    }
    const int id = IntOf(context, "id").value_or(0);
    if (aux->GetType("isDefault") == VTYPE_BOOL && aux->GetBool("isDefault")) {
      found->page = id;
      found->loaded = StringOf(context, "uniqueId");
      // A page of the extension's own: its scripts run in the page's world.
      if (StringOf(context, "origin") == "chrome-extension://" + extension) {
        found->world = id;
        found->own = true;
      }
    } else if (!found->own && ContentScriptWorld(context, extension)) {
      found->world = id;
    }
  }
  // Learn the id among Chromium's of each frame of `frames` that has none
  // yet, and with `documents` the id of the document it shows, where the
  // extension has a world to tell them; then go on.
  void Identified(CefRefPtr<TabInspector> inspector, int key, bool documents, std::shared_ptr<std::vector<Frame>> frames, std::function<void()> then) {
    auto left = std::make_shared<size_t>(1);
    auto finish = [left, then] { if (--*left == 0) then(); };
    for (size_t i = 0; i < frames->size(); ++i) {
      auto& frame = (*frames)[i];
      if (auto cached = frame_ids_.find({key, frame.devtools}); cached != frame_ids_.end()) {
        frame.id = cached->second.id;
        if (!frame.loaded.empty() && cached->second.loaded == frame.loaded) frame.document = cached->second.document;
      }
      if (!frame.world || (frame.id >= 0 && (!documents || !frame.document.empty()))) continue;
      ++*left;
      EvaluateIn(inspector, frame.session, frame.world, kFrameProbe, [this, key, frames, i, finish](std::optional<std::string> error, CefRefPtr<CefValue> value) {
        auto told = !error && value && value->GetType() == VTYPE_DICTIONARY ? value->GetDictionary() : nullptr;
        const auto id = IntOf(told, "frameId");
        auto& frame = (*frames)[i];
        // Only the main frame is 0.
        if (id && *id >= 0 && (*id > 0) == (frame.id != 0)) {
          frame.id = *id;
          frame.document = StringOf(told, "documentId");
          if (frame_ids_.size() >= kMaxFrames * 16) frame_ids_.clear();
          frame_ids_[{key, frame.devtools}] = KnownFrame{frame.id, frame.loaded, frame.document};
        }
        finish();
      }, false);
    }
    finish();
  }
  // The target on the pipe of each frame `query` names that the host met
  // before on a target of its own, among `targets`; empty unless it met
  // every one.
  std::vector<std::string> KnownTargets(int key, const FrameQuery& query, const std::vector<std::string>& targets) const {
    std::vector<std::string> named;
    auto find = [&](const std::function<bool(const KnownFrame&)>& matches) {
      for (const auto& [where, known] : frame_ids_)
        if (where.first == key && matches(known) && std::find(targets.begin(), targets.end(), where.second) != targets.end()) {
          if (std::find(named.begin(), named.end(), where.second) == named.end()) named.push_back(where.second);
          return true;
        }
      return false;
    };
    for (int id : query.ids) if (!find([id](const KnownFrame& known) { return known.id == id; })) return {};
    for (const auto& document : query.documents) if (!find([&](const KnownFrame& known) { return known.document == document; })) return {};
    return named;
  }
  // The frames under `local` that other sites' processes render, each a
  // target on the pipe: attached, with what runs in it, after `local`.
  void RemoteFrames(const std::string& extension, int key, FrameQuery query, std::shared_ptr<std::vector<Frame>> local, std::function<void(std::vector<Frame>)> then) {
    hooks_.pipe("Target.getTargets", nullptr, "", [this, extension, key, query, local, then](bool ok, CefRefPtr<CefDictionaryValue> result) {
      std::set<std::string> known;
      for (const auto& frame : *local) known.insert(frame.devtools);
      std::vector<std::string> targets;
      auto infos = ok && result ? result->GetList("targetInfos") : nullptr;
      // A frame of another site may hold one of a third: parents first.
      for (bool grew = true; grew && infos;) {
        grew = false;
        for (size_t i = 0; i < infos->GetSize() && known.size() < kMaxFrames; ++i) {
          auto info = infos->GetDictionary(i);
          const auto id = StringOf(info, "targetId");
          if (StringOf(info, "type") != "iframe" || known.count(id) || !known.count(StringOf(info, "parentFrameId"))) continue;
          known.insert(id);
          targets.push_back(id);
          grew = true;
        }
      }
      if (!query.all) {
        auto named = KnownTargets(key, query, targets);
        if (!named.empty()) targets = std::move(named);
      }
      if (targets.empty()) { then(*local); return; }
      auto remote = std::make_shared<std::vector<std::vector<Frame>>>(targets.size());
      auto left = std::make_shared<size_t>(targets.size());
      auto finish = [local, remote, left, then] {
        if (--*left) return;
        auto all = *local;
        for (const auto& frames : *remote) all.insert(all.end(), frames.begin(), frames.end());
        then(all);
      };
      const bool documents = !query.documents.empty();
      for (size_t i = 0; i < targets.size(); ++i) {
        auto params = CefDictionaryValue::Create();
        params->SetString("targetId", targets[i]);
        params->SetBool("flatten", true);
        hooks_.pipe("Target.attachToTarget", params, "", [this, extension, key, documents, target = targets[i], remote, i, finish](bool ok, CefRefPtr<CefDictionaryValue> result) {
          const auto session = ok && result ? StringOf(result, "sessionId") : std::string();
          if (session.empty()) { finish(); return; }
          auto frames = std::make_shared<std::vector<Frame>>();
          frames->push_back(Frame{target, -1, 0, 0, session});
          hooks_.pipe_listen(session, [frames, extension, session](CefRefPtr<CefDictionaryValue> context) { Place(*frames, context, extension, session, true); });
          hooks_.pipe("Runtime.enable", nullptr, session, [this, key, documents, session, frames, remote, i, finish](bool, CefRefPtr<CefDictionaryValue>) {
            hooks_.pipe_listen(session, nullptr);
            Identified(nullptr, key, documents, frames, [frames, remote, i, finish] {
              (*remote)[i] = *frames;
              finish();
            });
          });
        });
      }
    });
  }
  // Let go of the sessions on the pipe `frames` were reached by.
  void Release(const std::vector<Frame>& frames) {
    std::set<std::string> sessions;
    for (const auto& frame : frames) if (!frame.session.empty()) sessions.insert(frame.session);
    for (const auto& session : sessions) hooks_.pipe("Target.detachFromTarget", Session(session), "", [](bool, CefRefPtr<CefDictionaryValue>) {});
  }
  // Evaluate in a frame's context: by the tab's inspector, or on the pipe
  // for a frame another site's process renders.
  void EvaluateIn(CefRefPtr<TabInspector> inspector, const std::string& session, int context, const std::string& expression, Answer then, bool gesture = true) {
    if (session.empty()) {
      if (!inspector) { then(std::string("The page could not run the script"), nullptr); return; }
      Evaluate(inspector, context, expression, then, gesture);
      return;
    }
    auto params = CefDictionaryValue::Create();
    params->SetString("expression", expression);
    if (context) params->SetInt("contextId", context);
    params->SetBool("awaitPromise", true);
    params->SetBool("returnByValue", true);
    params->SetBool("userGesture", gesture);
    hooks_.pipe("Runtime.evaluate", params, session, [then](bool ok, CefRefPtr<CefDictionaryValue> result) {
      if (!ok || !result) { then(std::string("The page could not run the script"), nullptr); return; }
      if (auto thrown = result->GetDictionary("exceptionDetails")) {
        auto message = StringOf(thrown->GetDictionary("exception"), "description");
        then(message.empty() ? StringOf(thrown, "text") : message, nullptr);
        return;
      }
      auto remote = result->GetDictionary("result");
      then(std::nullopt, remote && remote->HasKey("value") ? remote->GetValue("value")->Copy() : nullptr);
    });
  }
  // The world the host made for the extension in the tab's page, where its
  // scripts get a content script's `chrome` (extension_content.js): made
  // once per page, so a later script and message find the listeners an
  // earlier one added.
  void ContentWorld(const std::string& extension, int caller, int tab, CefRefPtr<TabInspector> inspector, const std::string& frame, CefRefPtr<CefDictionaryValue> content, std::function<void(int)> then) {
    auto known = content_worlds_.find({tab, extension});
    if (known != content_worlds_.end()) { then(known->second); return; }
    // Whether messages and ports from the world can go to a worker.
    auto manifest = content->GetDictionary("manifest");
    auto background = manifest ? manifest->GetDictionary("background") : nullptr;
    if (StringOf(background, "service_worker").empty()) workerless_.insert(extension); else workerless_.erase(extension);
    FallbackWorld(inspector, frame, extension, [this, extension, caller, tab, inspector, content, then](int context) {
      if (!context) { then(0); return; }
      auto binding = CefDictionaryValue::Create();
      binding->SetString("name", "__tiloExtension");
      binding->SetString("executionContextName", "tilo-extension-" + extension);
      inspector->Call("Runtime.addBinding", binding, [this, extension, caller, tab, inspector, content, context, then](bool, CefRefPtr<CefDictionaryValue>) {
        inspector->Listen([this, tab](CefRefPtr<CefDictionaryValue> call) { FromContent(tab, call); });
        content->SetString("id", extension);
        Evaluate(inspector, context, "(" + std::string(kExtensionContent) + ")(" + ToJson(Wrap(content)) + ")",
                 [this, extension, caller, tab, context, then](std::optional<std::string> error, CefRefPtr<CefValue>) {
          if (error) { then(0); return; }
          bool first = true;
          for (const auto& [key, world] : content_worlds_) first = first && key.second != extension;
          content_worlds_[{tab, extension}] = context;
          // The worker tells of storage changes while the extension has
          // worlds to tell (extension_tabs.js); one asked from a page may
          // have stopped listening when the last went.
          if (first && caller && !workerless_.count(extension) && Has(extension, "storage"))
            InWorker(extension, "globalThis.__tiloExtensionTabs && __tiloExtensionTabs.watch()", false, [](std::optional<std::string>, CefRefPtr<CefValue>) {});
          then(context);
        });
      });
    });
  }
  // A call from a world the host made: a message for the extension, a port
  // to it, or its storage.
  void FromContent(int tab, CefRefPtr<CefDictionaryValue> call) {
    if (StringOf(call, "name") != "__tiloExtension") return;
    const int context = IntOf(call, "executionContextId").value_or(0);
    std::string extension;
    for (const auto& [key, world] : content_worlds_) if (key.first == tab && world == context) extension = key.second;
    // An extension's declared content scripts have Chromium's `chrome`; the
    // host gives their world only what its ports carry.
    bool declared = false;
    for (const auto& [key, world] : declared_worlds_) if (extension.empty() && key.first == tab && world == context) { extension = key.second; declared = true; }
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
    // Copied: it outlives the call it was read from.
    auto args = request->GetType("args") == VTYPE_DICTIONARY ? request->GetDictionary("args")->Copy(false) : nullptr;
    const auto method = StringOf(request, "method");
    if (declared && method != "port") return;
    if (method == "port" && args) {
      const auto local = StringOf(args, "port");
      for (const auto& [port, open] : ports_)
        if (open.tab == tab && open.context == context && open.local == local) {
          PortOp(std::string(port), kPageEnd, StringOf(args, "op") == "disconnect", args->GetValue("message"));
          return;
        }
    } else if (method == "connect" && args) {
      // The port exists from now: what the script posts on it waits until
      // the extension's ends said whether they take it.
      const auto local = StringOf(args, "port");
      if (local.empty() || local.size() > 64 || ports_.size() >= kMaxPorts) return;
      const std::string port = "p" + std::to_string(++port_);
      ports_[port] = Port{extension, local, tab, context, {}, true, {}};
      const auto name = StringOf(args, "name");
      Ensure([this, extension, tab, port, name] {
        const auto tabs = Tabs();
        const auto* from = Find(tabs, tab);
        if (!from || !ports_.count(port)) { Settled(extension, port, {}); return; }
        Connect(extension, port, name, Sender(extension, *from));
      });
    } else if (method == "sendMessage" && args) {
      Ensure([this, extension, tab, args, settle, failure] {
        const auto tabs = Tabs();
        const auto* from = Find(tabs, tab);
        if (!from) { failure(kNoReceiver); return; }
        ToExtension(extension, args->HasKey("message") ? args->GetValue("message") : nullptr, Sender(extension, *from),
                    [settle, failure](std::optional<std::string> error, CefRefPtr<CefValue> value) {
          if (error) { failure(*error); return; }
          settle(true, value);
        });
      });
    } else if (method == "storage" && args) {
      static const std::set<std::string> areas{"local", "sync", "session"};
      static const std::set<std::string> ops{"get", "set", "remove", "clear", "getBytesInUse"};
      const auto area = StringOf(args, "area");
      const auto op = StringOf(args, "op");
      auto values = args->GetType("args") == VTYPE_LIST ? args->GetList("args") : CefListValue::Create();
      if (!areas.count(area) || !ops.count(op)) { failure("Unknown storage call"); return; }
      // Its worker, or the page of an extension without one.
      const auto ends = Ends(extension);
      if (ends.empty()) { failure("The extension is not running"); return; }
      AtEnd(extension, ends.front(), "chrome.storage." + area + "." + op + "(..." + ToJson(Wrap(values)) + ")",
            [settle, failure](std::optional<std::string> error, CefRefPtr<CefValue> value) {
        if (error) failure(*error); else settle(true, value);
      });
    } else {
      failure("Unknown call");
    }
  }
  // Deliver a message to `chrome.runtime.onMessage` where the expression runs,
  // answered as Chrome answers it: by the first reply, or with nothing
  // (`none` where no listener heard it).
  static std::string Dispatch(CefRefPtr<CefValue> message, CefRefPtr<CefDictionaryValue> sender) {
    return "new Promise((resolve) => {"
           "  if (!chrome.runtime.onMessage.hasListeners()) { resolve({none: true}); return; }"
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
  // The language the page says it is in, as Chrome names languages.
  void DetectLanguage(const ExtensionTab& tab, Reply reply) {
    auto inspector = hooks_.inspector(tab.tab);
    if (!inspector) { Fail(reply, "No tab with id: " + std::to_string(tab.id) + "."); return; }
    Evaluate(inspector, 0, "document.documentElement ? document.documentElement.lang : ''", [reply](std::optional<std::string>, CefRefPtr<CefValue> value) {
      auto code = CefValue::Create();
      code->SetString(LanguageCode(value && value->GetType() == VTYPE_STRING ? value->GetString().ToString() : std::string()));
      Succeed(reply, code);
    }, false);
  }
  // Chrome's code for a language tag: the language alone ("de" for "de-AT"),
  // Chinese with its region ("zh-CN", "zh-TW"), and "und" for none.
  static std::string LanguageCode(std::string tag) {
    for (auto& c : tag) c = c == '_' ? '-' : static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    const auto primary = tag.substr(0, tag.find('-'));
    if (primary.size() < 2 || primary.size() > 3) return "und";
    for (char c : primary) if (c < 'a' || c > 'z') return "und";
    if (primary != "zh") return primary;
    for (const char* traditional : {"-hant", "-tw", "-hk", "-mo"}) if (tag.find(traditional) != std::string::npos) return "zh-TW";
    return "zh-CN";
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
  // A message for the extension's scripts in the tab, answered as Chrome
  // answers it: by the first reply, or with nothing.
  void SendMessage(const std::string& extension, const ExtensionTab& tab, CefRefPtr<CefValue> reference, std::optional<int> frame_id, const std::string& document, Reply reply) {
    // A value read from the request is valid only while the request is.
    auto message = reference ? reference->Copy() : CefValue::Create();
    auto inspector = hooks_.inspector(tab.tab);
    if (!inspector) { Fail(reply, kNoReceiver); return; }
    auto sender = CefDictionaryValue::Create();
    sender->SetString("id", extension);
    sender->SetString("origin", "chrome-extension://" + extension);
    auto answer = [reply](std::optional<std::string> error, CefRefPtr<CefValue> value) {
      if (error) { Fail(reply, *error); return; }
      auto result = value && value->GetType() == VTYPE_DICTIONARY ? value->GetDictionary() : nullptr;
      // No listener in the page: the extension is told, as Chrome tells it,
      // and may inject its script then.
      if (result && result->HasKey("none")) { Fail(reply, kNoReceiver); return; }
      Succeed(reply, result && result->HasKey("reply") ? result->GetValue("reply")->Copy() : nullptr);
    };
    const std::string expression = Dispatch(message, sender);
    const int key = tab.tab;
    // One frame: its content scripts, or the extension's own page there,
    // which an extension names by the id or the document a message from
    // there gave it (`sender.frameId`, `sender.documentId`).
    FrameQuery query;
    if (frame_id && (*frame_id != 0 || !document.empty())) query.ids.push_back(*frame_id);
    if (!document.empty()) query.documents.push_back(document);
    const bool main = query.MainOnly();
    Frames(extension, tab, query, [this, inspector, extension, key, message, sender, expression, answer, frame_id, document, main, reply](std::vector<Frame> frames) {
      const Frame* frame = nullptr;
      for (const auto& entry : frames) {
        const bool named = main ? entry.id == 0 : (!frame_id || entry.id == *frame_id) && (document.empty() || entry.document == document);
        if (named && !frame) frame = &entry;
      }
      if (frame && frame->world) {
        EvaluateIn(inspector, frame->session, frame->world, expression, [this, frames, answer](std::optional<std::string> error, CefRefPtr<CefValue> value) {
          Release(frames);
          answer(error, value);
        });
        return;
      }
      Release(frames);
      // Its script injected on demand listens in the world the host made.
      auto made = main ? content_worlds_.find({key, extension}) : content_worlds_.end();
      if (made == content_worlds_.end()) { Fail(reply, kNoReceiver); return; }
      Evaluate(inspector, made->second, "__tiloExtensionContent.deliver(" + ToJson(message) + ", " + ToJson(Wrap(sender)) + ")", answer);
    });
  }

  // Who a message or a port from a script in a world the host made comes
  // from, as Chrome tells a content script's.
  CefRefPtr<CefDictionaryValue> Sender(const std::string& extension, const ExtensionTab& from) const {
    auto sender = CefDictionaryValue::Create();
    sender->SetString("id", extension);
    sender->SetString("url", from.url);
    sender->SetInt("frameId", 0);
    sender->SetDictionary("tab", TabObject(extension, from));
    return sender;
  }
  // The extension's own pages the host shows (its popup, its settings), by
  // tab, in the host's order.
  std::vector<int> Pages(const std::string& extension) const {
    std::vector<int> pages;
    const std::string prefix = "chrome-extension://" + extension + "/";
    for (const auto& tab : hooks_.tabs()) if (tab.url.rfind(prefix, 0) == 0) pages.push_back(tab.tab);
    return pages;
  }
  // Where a script in a page reaches the extension: its worker and its pages.
  std::vector<int> Ends(const std::string& extension) const {
    std::vector<int> ends;
    if (!workerless_.count(extension)) ends.push_back(0);
    for (int page : Pages(extension)) ends.push_back(page);
    return ends;
  }
  // Evaluate `expression` at one of the extension's ends.
  void AtEnd(const std::string& extension, int end, const std::string& expression, Answer answer) {
    if (!end) { InWorker(extension, expression, false, std::move(answer)); return; }
    auto inspector = hooks_.inspector(end);
    if (!inspector) { answer(std::string("The page is gone"), nullptr); return; }
    Evaluate(inspector, 0, expression, std::move(answer));
  }
  // A message from a script in a world the host made, for every listener of
  // the extension, in its worker and its open pages alike: answered by the
  // first reply, with nothing when none came, and refused when no listener
  // heard it.
  void ToExtension(const std::string& extension, CefRefPtr<CefValue> message, CefRefPtr<CefDictionaryValue> sender, Answer answer) {
    const auto ends = Ends(extension);
    if (ends.empty()) { answer(std::string(kNoReceiver), nullptr); return; }
    struct Round { size_t left; bool done = false, heard = false; Answer answer; };
    auto round = std::make_shared<Round>(Round{ends.size(), false, false, std::move(answer)});
    const auto expression = Dispatch(message, sender);
    for (int end : ends) AtEnd(extension, end, expression, [round](std::optional<std::string> error, CefRefPtr<CefValue> value) {
      if (round->done) return;
      auto result = !error && value && value->GetType() == VTYPE_DICTIONARY ? value->GetDictionary() : nullptr;
      if (result && result->HasKey("reply")) {
        round->done = true;
        round->answer(std::nullopt, result->GetValue("reply")->Copy());
        return;
      }
      if (result && !result->HasKey("none")) round->heard = true;
      if (--round->left) return;
      round->done = true;
      if (round->heard) round->answer(std::nullopt, nullptr); else round->answer(std::string(kNoReceiver), nullptr);
    });
  }

  // A script in a page opened `port` (runtime.connect): every end of the
  // extension with an onConnect listener takes it.
  void Connect(const std::string& extension, const std::string& port, const std::string& name, CefRefPtr<CefDictionaryValue> sender) {
    const auto ends = Ends(extension);
    if (ends.empty()) { Settled(extension, port, {}); return; }
    auto left = std::make_shared<size_t>(ends.size());
    auto took = std::make_shared<std::set<int>>();
    auto id = CefValue::Create();
    id->SetString(port);
    auto label = CefValue::Create();
    label->SetString(name);
    const std::string expression = "globalThis.__tiloExtensionTabs ? __tiloExtensionTabs.connected(" + ToJson(id) + ", " + ToJson(label) + ", " + ToJson(Wrap(sender)) + ") : false";
    for (int end : ends) AtEnd(extension, end, expression, [this, extension, port, end, left, took](std::optional<std::string> error, CefRefPtr<CefValue> value) {
      if (!error && value && value->GetType() == VTYPE_BOOL && value->GetBool()) took->insert(end);
      if (--*left == 0) Settled(extension, port, *took);
    });
  }
  // The extension opened `port` to the tab (tabs.connect): the script it
  // injected there takes it if it listens for one.
  void Open(const std::string& extension, int caller, CefRefPtr<CefDictionaryValue> request) {
    // Copied: it outlives the request it was read from.
    auto op = request->Copy(false);
    const auto port = StringOf(op, "port");
    if (port.size() < 2 || port.size() > 64 || port[0] != 'x' || ports_.count(port) || ports_.size() >= kMaxPorts) return;
    ports_[port] = Port{extension, port, 0, 0, {caller}, true, {}};
    const auto tabs = Tabs();
    const auto* tab = ByExtensionId(tabs, IntOf(op, "tabId"));
    auto inspector = tab ? hooks_.inspector(tab->tab) : nullptr;
    if (!inspector) { Refused(port); return; }
    ports_[port].tab = tab->tab;
    auto made = content_worlds_.find({tab->tab, extension});
    if (made != content_worlds_.end()) { Offer(extension, caller, made->second, op); return; }
    // Its declared content scripts' world, which the host gives what a port
    // needs once.
    const int key = tab->tab;
    NamedWorld(extension, *tab, [this, extension, caller, key, inspector, op](int world, std::string, std::string name) {
      if (!world || name.empty() || !ports_.count(StringOf(op, "port"))) { Refused(StringOf(op, "port")); return; }
      auto known = declared_worlds_.find({key, extension});
      if (known != declared_worlds_.end() && known->second == world) { Offer(extension, caller, world, op); return; }
      auto binding = CefDictionaryValue::Create();
      binding->SetString("name", "__tiloExtension");
      binding->SetString("executionContextName", name);
      inspector->Call("Runtime.addBinding", binding, [this, extension, caller, key, inspector, world, op](bool, CefRefPtr<CefDictionaryValue>) {
        inspector->Listen([this, key](CefRefPtr<CefDictionaryValue> call) { FromContent(key, call); });
        auto content = CefDictionaryValue::Create();
        content->SetString("id", extension);
        content->SetBool("declared", true);
        Evaluate(inspector, world, "(" + std::string(kExtensionContent) + ")(" + ToJson(Wrap(content)) + ")",
                 [this, extension, caller, key, world, op](std::optional<std::string> error, CefRefPtr<CefValue>) {
          if (error) { Refused(StringOf(op, "port")); return; }
          declared_worlds_[{key, extension}] = world;
          Offer(extension, caller, world, op);
        });
      });
    });
  }
  // Offer the extension's port to the script world `context` in its tab.
  void Offer(const std::string& extension, int caller, int context, CefRefPtr<CefDictionaryValue> op) {
    const auto port = StringOf(op, "port");
    auto found = ports_.find(port);
    if (found == ports_.end()) return;
    auto inspector = hooks_.inspector(found->second.tab);
    if (!inspector) { Refused(port); return; }
    found->second.context = context;
    const auto tabs = Tabs();
    auto sender = CefDictionaryValue::Create();
    sender->SetString("id", extension);
    sender->SetString("origin", "chrome-extension://" + extension);
    if (const auto* page = Find(tabs, caller)) sender->SetString("url", page->url);
    auto id = CefValue::Create();
    id->SetString(port);
    auto label = CefValue::Create();
    label->SetString(StringOf(op, "name"));
    Evaluate(inspector, context, "__tiloExtensionContent.connect(" + ToJson(id) + ", " + ToJson(label) + ", " + ToJson(Wrap(sender)) + ")",
             [this, extension, port, caller](std::optional<std::string> error, CefRefPtr<CefValue> value) {
      if (!error && value && value->GetType() == VTYPE_BOOL && value->GetBool()) Settled(extension, port, {caller});
      else Refused(port);
    });
  }
  // Nothing in the page took the extension's port.
  void Refused(std::string port) {
    auto found = ports_.find(port);
    if (found == ports_.end()) return;
    auto open = std::move(found->second);
    ports_.erase(found);
    for (int end : open.ends) ToEnd(open.extension, end, port, "disconnect", nullptr, kNoReceiver);
  }
  // The port is open between the page and the `ends` that took it, or, with
  // none, closed again.
  void Settled(const std::string& extension, std::string port, std::set<int> ends) {
    auto found = ports_.find(port);
    if (found == ports_.end()) {
      // Its page went meanwhile.
      for (int end : ends) ToEnd(extension, end, port, "disconnect", nullptr, "");
      return;
    }
    auto& open = found->second;
    if (ends.empty()) {
      ToPage(open, "disconnect", nullptr, kNoReceiver);
      ports_.erase(found);
      return;
    }
    open.ends = std::move(ends);
    open.opening = false;
    auto early = std::move(open.early);
    for (auto& sent : early) PortOp(port, sent.from, sent.disconnect, sent.message);
  }
  // A port's message or its closing, from the page's end (kPageEnd) or one of
  // the extension's.
  void PortOp(std::string port, int from, bool disconnect, CefRefPtr<CefValue> reference) {
    auto found = ports_.find(port);
    if (found == ports_.end()) return;
    auto& open = found->second;
    // A value read from a request is valid only while the request is.
    auto message = reference ? reference->Copy() : CefValue::Create();
    if (open.opening) {
      if (open.early.size() < 1024) open.early.push_back({from, disconnect, message});
      return;
    }
    if (from == kPageEnd) {
      if (disconnect) {
        auto closed = std::move(open);
        ports_.erase(found);
        for (int end : closed.ends) ToEnd(closed.extension, end, port, "disconnect", nullptr, "");
        return;
      }
      for (int end : open.ends) ToEnd(open.extension, end, port, "message", message, "");
      return;
    }
    if (!open.ends.count(from)) return;
    if (disconnect) { Gone(port, from); return; }
    ToPage(open, "message", message, "");
  }
  // What an extension's page or worker sends on its ports.
  void FromEnd(const std::string& extension, int caller, CefRefPtr<CefDictionaryValue> op) {
    const auto kind = StringOf(op, "op");
    if (kind == "open") { Open(extension, caller, op); return; }
    const auto port = StringOf(op, "port");
    auto found = ports_.find(port);
    if (found == ports_.end() || found->second.extension != extension) return;
    if (kind == "post" || kind == "disconnect") PortOp(port, caller, kind == "disconnect", op->GetValue("message"));
  }
  // One of the extension's ends no longer holds the port: the page's end
  // closes with the last.
  void Gone(std::string port, int end) {
    auto found = ports_.find(port);
    if (found == ports_.end() || !found->second.ends.erase(end) || !found->second.ends.empty()) return;
    ToPage(found->second, "disconnect", nullptr, "");
    ports_.erase(found);
  }
  void ToPage(const Port& open, const std::string& op, CefRefPtr<CefValue> message, const std::string& error) {
    auto inspector = hooks_.inspector(open.tab);
    if (!inspector || !open.context) return;
    auto values = CefListValue::Create();
    values->SetString(0, open.local);
    values->SetString(1, op);
    if (message) values->SetValue(2, message->Copy()); else values->SetNull(2);
    values->SetString(3, error);
    Evaluate(inspector, open.context, "__tiloExtensionContent.port(..." + ToJson(Wrap(values)) + ")", [](std::optional<std::string>, CefRefPtr<CefValue>) {});
  }
  // What the page's ends of ports send to one of the extension's ends goes
  // in order, gathered: while one delivery is under way the next collects
  // all that follows, so a burst wakes the worker once and costs it one
  // evaluation, where one apiece overran the worker's queue and lost the
  // port. It leaves on a later turn, so nothing a caller iterates changes
  // under it.
  void ToEnd(const std::string& extension, int end, const std::string& port, const std::string& op, CefRefPtr<CefValue> message, const std::string& error) {
    auto values = CefListValue::Create();
    values->SetString(0, port);
    values->SetString(1, op);
    if (message) values->SetValue(2, message->Copy()); else values->SetNull(2);
    values->SetString(3, error);
    auto& box = outboxes_[{extension, end}];
    box.ops.push_back(Outgoing{port, op == "disconnect", ToJson(Wrap(values))});
    if (box.sending) return;
    box.sending = true;
    std::weak_ptr<bool> alive = alive_;
    CefPostTask(TID_UI, base::BindOnce([](ExtensionTabs* self, std::weak_ptr<bool> alive, std::string extension, int end) {
      if (!alive.expired()) self->Flush(extension, end);
    }, this, alive, extension, end));
  }
  // Deliver what gathered for the end, then what gathers meanwhile. An end
  // that no longer knows a port (its worker slept since) drops it.
  void Flush(const std::string& extension, int end) {
    auto found = outboxes_.find({extension, end});
    if (found == outboxes_.end()) return;
    auto& queued = found->second.ops;
    if (queued.empty()) { outboxes_.erase(found); return; }
    std::vector<Outgoing> ops;
    std::string expression = "(() => { const tabs = globalThis.__tiloExtensionTabs; return tabs ? [";
    size_t taken = 0;
    while (taken < queued.size() && (!taken || expression.size() + queued[taken].values.size() < kMaxPortBatch)) {
      expression += (taken ? ", tabs.port(..." : "tabs.port(...") + queued[taken].values + ")";
      ops.push_back(std::move(queued[taken++]));
    }
    queued.erase(queued.begin(), queued.begin() + static_cast<std::ptrdiff_t>(taken));
    expression += "] : false; })()";
    AtEnd(extension, end, expression, [this, extension, end, ops](std::optional<std::string> error, CefRefPtr<CefValue> value) {
      auto known = !error && value && value->GetType() == VTYPE_LIST ? value->GetList() : nullptr;
      for (size_t i = 0; i < ops.size(); ++i) {
        if (ops[i].closing) continue;
        if (!known || i >= known->GetSize() || known->GetType(i) != VTYPE_BOOL || !known->GetBool(i)) Gone(ops[i].port, end);
      }
      Flush(extension, end);
    });
  }
  // The tab's document went: the worlds the host made in it, the ports to
  // them, and the ports its extension page held.
  void Drop(int tab) {
    for (auto it = content_worlds_.begin(); it != content_worlds_.end();) it = it->first.first == tab ? content_worlds_.erase(it) : std::next(it);
    for (auto it = frame_ids_.begin(); it != frame_ids_.end();) it = it->first.first == tab ? frame_ids_.erase(it) : std::next(it);
    for (auto it = declared_worlds_.begin(); it != declared_worlds_.end();) it = it->first.first == tab ? declared_worlds_.erase(it) : std::next(it);
    std::vector<std::string> held;
    for (auto it = ports_.begin(); it != ports_.end();) {
      if (it->second.tab != tab) {
        if (it->second.ends.count(tab)) held.push_back(it->first);
        ++it;
        continue;
      }
      auto closed = std::move(it->second);
      const auto port = it->first;
      it = ports_.erase(it);
      for (int end : closed.ends) if (end != tab) ToEnd(closed.extension, end, port, "disconnect", nullptr, "");
    }
    for (const auto& port : held) Gone(port, tab);
  }

  // Tell the extension's service worker of an event: now if it runs, after
  // waking it if it sleeps. A click reaches it whether or not it said it
  // listens, as Chrome starts a worker for one.
  void Push(const std::string& extension, const std::string& event, CefRefPtr<CefListValue> args, bool always) {
    if (!always && !(listening_.count(extension) && listening_[extension].count(event))) return;
    auto name = CefValue::Create();
    name->SetString(event);
    InWorker(extension, "globalThis.__tiloExtensionTabs ? __tiloExtensionTabs.dispatch(" + ToJson(name) + ", " + ToJson(Wrap(args)) + ") : -1",
             event == "action.onClicked" || event == "commands.onCommand" || event == "contextMenus.onClicked",
             [](std::optional<std::string>, CefRefPtr<CefValue>) {});
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

  // The toolbar button's look (`chrome.action`), which the Browser draws.
  // For every tab, Chromium keeps it and answers for it, and the script set
  // it there first: the host tells the Browser, every tab of it. For one
  // tab, Chromium finds no such tab, so the host keeps it (an icon is only
  // shown) until the tab's next document or its close, as Chrome does, and
  // tells the Browser for that tab.
  void Button(const std::string& extension, const std::vector<ExtensionTab>& tabs, bool get, CefRefPtr<CefDictionaryValue> args, const Reply& reply) {
    const auto key = StringOf(args, "key");
    if (key != "text" && key != "title" && key != "popup" && key != "enabled" && key != "icon") { Fail(reply, "Unknown action property"); return; }
    const bool for_tab = IntOf(args, "tabId").has_value();
    const auto* tab = for_tab ? ByExtensionId(tabs, IntOf(args, "tabId")) : nullptr;
    if (for_tab && !tab) { Fail(reply, "No tab with id: " + std::to_string(IntOf(args, "tabId").value_or(0)) + "."); return; }
    if (get) {
      if (!tab) { Fail(reply, "Chromium answers for every tab"); return; }
      CefRefPtr<CefValue> result;
      auto found = action_states_.find({extension, tab->id});
      if (found != action_states_.end()) if (auto entry = found->second.find(key); entry != found->second.end()) result = entry->second->Copy();
      Succeed(reply, result);
      return;
    }
    auto value = args->HasKey("value") ? args->GetValue("value") : nullptr;
    if (!value || value->GetType() == VTYPE_INVALID) { value = CefValue::Create(); value->SetNull(); }
    if (!ButtonValue(extension, key, value)) { Fail(reply, "Invalid value for " + key); return; }
    // The Browser names the text the badge.
    const std::string name = key == "text" ? "badge" : key;
    auto change = CefDictionaryValue::Create();
    change->SetValue(name, value->Copy());
    if (tab) {
      if (key != "icon") {
        const std::pair<std::string, int> owner{extension, tab->id};
        if (value->GetType() != VTYPE_NULL) action_states_[owner][key] = value->Copy();
        else if (auto found = action_states_.find(owner); found != action_states_.end()) {
          found->second.erase(key);
          if (found->second.empty()) action_states_.erase(found);
        }
      }
      buttons_on_.insert(tab->tab);
      TellButton(tab->tab, extension, false, change);
    } else {
      auto& values = buttons_[extension];
      if (!values) values = CefDictionaryValue::Create();
      if (value->GetType() == VTYPE_NULL) values->Remove(name);
      else values->SetValue(name, value->Copy());
      for (const auto& each : tabs) if (!each.panel) TellButton(each.tab, extension, true, change);
    }
    Succeed(reply, nullptr);
  }
  // A value of the button's of its own kind and size, a popup only among the
  // extension's own pages; null takes one back.
  static bool ButtonValue(const std::string& extension, const std::string& key, CefRefPtr<CefValue> value) {
    const auto type = value->GetType();
    if (type == VTYPE_NULL) return true;
    if (key == "enabled") return type == VTYPE_BOOL;
    if (key == "icon") return type == VTYPE_DICTIONARY && ButtonIcon(value->GetDictionary());
    if (type != VTYPE_STRING) return false;
    const auto text = value->GetString().ToString();
    if (key == "popup") return text.size() <= 2048 && (text.empty() || text.rfind("chrome-extension://" + extension + "/", 0) == 0);
    return text.size() <= 1024;
  }
  // An icon by its path among the extension's files, or its pixels: RGBA,
  // at most 128 pixels a side, in base64.
  static bool ButtonIcon(CefRefPtr<CefDictionaryValue> icon) {
    if (icon->GetSize() == 1 && icon->GetType("path") == VTYPE_STRING) {
      const auto path = icon->GetString("path").ToString();
      return !path.empty() && path.size() <= 2048 && path[0] == '/';
    }
    const auto width = IntOf(icon, "width"), height = IntOf(icon, "height");
    if (icon->GetSize() != 3 || !width || !height || *width < 1 || *height < 1 || *width > 128 || *height > 128 || icon->GetType("data") != VTYPE_STRING) return false;
    const size_t bytes = static_cast<size_t>(*width) * static_cast<size_t>(*height) * 4;
    return icon->GetString("data").ToString().size() == (bytes + 2) / 3 * 4;
  }
  void TellButton(int tab, const std::string& extension, bool global, CefRefPtr<CefDictionaryValue> change) {
    if (!hooks_.event) return;
    auto fields = CefDictionaryValue::Create();
    fields->SetString("id", extension);
    fields->SetBool("global", global);
    fields->SetDictionary("change", change->Copy(false));
    hooks_.event(tab, "extension_action", fields);
  }

  Hooks hooks_;
  // What each extension set for its button on every tab, and the tabs on
  // which one set something for that tab alone.
  std::map<std::string, CefRefPtr<CefDictionaryValue>> buttons_;
  std::set<int> buttons_on_;
  std::map<std::string, Access> access_;
  std::map<std::string, std::set<std::string>> listening_;
  std::set<std::pair<std::string, int>> granted_;
  // Each button's look for one tab, by extension and tab id.
  std::map<std::pair<std::string, int>, std::map<std::string, CefRefPtr<CefValue>>> action_states_;
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
  // Each frame's id among Chromium's and its document's, by tab and the
  // inspector's frame id.
  std::map<std::pair<int, std::string>, KnownFrame> frame_ids_;
  // When each extension last captured a tab, within the last second.
  std::map<std::string, std::deque<std::chrono::steady_clock::time_point>> captures_;
  // The worlds of extensions' declared content scripts the host gave ports.
  std::map<std::pair<int, std::string>, int> declared_worlds_;
  // Extensions whose manifest names no worker: their pages alone answer.
  std::set<std::string> workerless_;
  std::map<std::string, Port> ports_;
  // By extension and end (0 for its worker, else its page's tab).
  std::map<std::pair<std::string, int>, Outbox> outboxes_;
  uint64_t port_ = 0;
  uint64_t generation_ = 0;
  ExtensionMenus menus_;
#if defined(OS_MAC)
  ExtensionCommands commands_{true};
#else
  ExtensionCommands commands_{false};
#endif
  bool saving_ = false;
  std::shared_ptr<bool> alive_ = std::make_shared<bool>(true);
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
