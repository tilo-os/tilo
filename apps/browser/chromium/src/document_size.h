#pragma once
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <cmath>
#include <functional>
#include <string>

namespace tilo {
// How tall the page's document is and where it is scrolled, for an embedder
// that sizes the view to its content (a mail reader scrolling as one page).
// Read from the browser process over DevTools after a paint, so the page can
// neither observe nor answer it; at most one read is in flight, and a paint
// during it reads once more after. Reported only when it changed.
class DocumentSize final : public CefDevToolsMessageObserver {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  DocumentSize(CefRefPtr<CefBrowser> browser,Emit emit):browser_(browser),emit_(std::move(emit)) {}
  void Attach() { registration_=browser_->GetHost()->AddDevToolsMessageObserver(this); }
  void Detach() { registration_=nullptr;browser_=nullptr;pending_=0;again_=false; }
  // Off by default: a tab nobody sizes to its content costs nothing.
  void Enable(bool enabled) {
    enabled_=enabled;
    if(!enabled) { again_=false;width_=height_=scroll_y_=-1;return; }
    Painted();
  }
  void Painted() {
    if(!enabled_ || !browser_) return;
    if(pending_) { again_=true;return; }
    pending_=browser_->GetHost()->ExecuteDevToolsMethod(0,"Page.getLayoutMetrics",nullptr);
  }
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>,int id,bool success,const void* result,size_t size) override {
    // CEF may answer before ExecuteDevToolsMethod returns the id, and every
    // observer sees every result: copy, then match ids on the next UI turn.
    CefRefPtr<DocumentSize> self(this);
    std::string copy(static_cast<const char*>(result),size);
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<DocumentSize> self,int id,bool success,std::string copy) {
      self->Result(id,success,copy);
    },self,id,success,std::move(copy)));
  }
  void OnDevToolsAgentDetached(CefRefPtr<CefBrowser>) override { pending_=0;again_=false; }

 private:
  static double Field(CefRefPtr<CefDictionaryValue> d,const char* key) {
    if(!d) return -1;
    auto type=d->GetType(key);
    double value=type==VTYPE_DOUBLE?d->GetDouble(key):type==VTYPE_INT?d->GetInt(key):-1;
    return std::isfinite(value) && value>=0 && value<=1e7?value:-1;
  }
  void Result(int id,bool success,const std::string& json) {
    if(!browser_ || id!=pending_) return;
    pending_=0;
    if(success && enabled_) {
      auto parsed=CefParseJSON(json,JSON_PARSER_RFC);
      auto result=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
      // CSS pixels, as the page lays out: the content at least fills the view.
      auto content=result?result->GetDictionary("cssContentSize"):nullptr;
      auto viewport=result?result->GetDictionary("cssVisualViewport"):nullptr;
      double width=Field(content,"width"),height=Field(content,"height"),scroll_y=Field(viewport,"pageY");
      if(width>=0 && height>=0 && scroll_y>=0 && (width!=width_ || height!=height_ || scroll_y!=scroll_y_)) {
        width_=width;height_=height;scroll_y_=scroll_y;
        auto fields=CefDictionaryValue::Create();
        fields->SetDouble("width",width);fields->SetDouble("height",height);fields->SetDouble("scroll_y",scroll_y);
        emit_("document_size",fields);
      }
    }
    // Never execute a nested DevTools method in its result callback.
    if(again_) {
      again_=false;
      CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<DocumentSize> self) { self->Painted(); },CefRefPtr<DocumentSize>(this)));
    }
  }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;Emit emit_;
  bool enabled_=false,again_=false;int pending_=0;
  double width_=-1,height_=-1,scroll_y_=-1;
  IMPLEMENT_REFCOUNTING(DocumentSize);
};
}
