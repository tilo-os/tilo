#pragma once
#include "include/cef_jsdialog_handler.h"
#include "include/cef_values.h"
#include <functional>
#include <map>
#include <string>

namespace tilo {
// A page's `alert`, `confirm`, `prompt` and before-unload question. CEF's
// default implementation needs a native window handle, which a composited
// window has none of, so the callback is held here and the page stays paused
// until an answer arrives. Every open dialog also freezes that tab's renderer:
// what the tab reports while one is open is what the page said before it
// started waiting, not what it says now.
class JsDialogs final : public CefJSDialogHandler {
 public:
  using Emit = std::function<void(const std::string&, CefRefPtr<CefDictionaryValue>)>;
  using Opened = std::function<void(const std::string&)>;
  using Closed = std::function<void()>;
  JsDialogs(Emit emit, Opened opened, Closed closed)
      : emit_(std::move(emit)), opened_(std::move(opened)), closed_(std::move(closed)) {}
  bool OnJSDialog(CefRefPtr<CefBrowser>, const CefString& origin_url, JSDialogType type,
                  const CefString& message, const CefString& default_prompt,
                  CefRefPtr<CefJSDialogCallback> callback, bool& suppress) override {
    suppress = false;
    const char* kind = nullptr;
    switch (type) {
      case JSDIALOGTYPE_ALERT: kind = "alert"; break;
      case JSDIALOGTYPE_CONFIRM: kind = "confirm"; break;
      case JSDIALOGTYPE_PROMPT: kind = "prompt"; break;
      default: return false;
    }
    if (!Open(kind, origin_url, message, default_prompt, callback)) {
      // Too many already: answering "no" keeps the page moving rather than
      // leaving it waiting on a dialog nobody will see.
      callback->Continue(false, CefString());
    }
    return true;
  }
  bool OnBeforeUnloadDialog(CefRefPtr<CefBrowser>, const CefString& message, bool,
                            CefRefPtr<CefJSDialogCallback> callback) override {
    if (!Open("beforeunload", CefString(), message, CefString(), callback))
      callback->Continue(false, CefString());
    return true;
  }
  // CEF dismisses every dialog itself (a new navigation, a reload, the tab
  // going away). The page has already been let go by then, so the question is
  // withdrawn rather than answered.
  void OnResetDialogState(CefRefPtr<CefBrowser>) override { Forget(); }
  void OnDialogClosed(CefRefPtr<CefBrowser>) override { Forget(); }
  // The answer to the dialog `id`: `accept` is its OK button, and `text` is a
  // prompt's value. An id no longer open is ignored, since the page moved on.
  void Decide(int id, bool accept, const std::string& text) {
    auto found = dialogs_.find(id);
    if (found == dialogs_.end()) return;
    auto callback = found->second;
    dialogs_.erase(found);
    callback->Continue(accept, text);
    Publish(id);
    if (dialogs_.empty() && closed_) closed_();
  }
  // Every open dialog ends as "no": the tab is closing or navigating away.
  void Close() {
    while (!dialogs_.empty()) Decide(dialogs_.begin()->first, false, std::string());
  }
  bool Open() const { return !dialogs_.empty(); }

 private:
  bool Open(const char* kind, const CefString& origin, const CefString& message,
            const CefString& default_prompt, CefRefPtr<CefJSDialogCallback> callback) {
    if (dialogs_.size() >= 8) return false;
    const int id = ++sequence_;
    dialogs_[id] = callback;
    auto d = CefDictionaryValue::Create();
    d->SetInt("id", id);
    d->SetString("kind", kind);
    d->SetString("message", message);
    d->SetString("default_prompt", default_prompt);
    d->SetString("origin", origin);
    // The tab's page actions stop here, and the request that was waiting is
    // told why now rather than at its timeout.
    if (opened_) opened_(kind);
    emit_("js_dialog", d);
    return true;
  }
  void Publish(int id) {
    auto d = CefDictionaryValue::Create();
    d->SetInt("id", id);
    emit_("js_dialog_closed", d);
  }
  // The dialog CEF reset is the one it took from us last.
  void Forget() {
    if (dialogs_.empty()) return;
    const int id = dialogs_.rbegin()->first;
    dialogs_.erase(id);
    Publish(id);
    if (dialogs_.empty() && closed_) closed_();
  }
  Emit emit_; Opened opened_; Closed closed_;
  int sequence_ = 0;
  std::map<int, CefRefPtr<CefJSDialogCallback>> dialogs_;
  IMPLEMENT_REFCOUNTING(JsDialogs);
};
}
