#pragma once
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <functional>
#include <map>
#include <string>
#include <utility>

namespace tilo {
// The page as layout rather than pixels, for the paint stream: a snapshot of
// every box and text line the engine laid out, and the bytes of a resource the
// page already loaded. Both come from the browser process over DevTools, so
// the page can neither observe nor answer them, and a resource is read from
// the engine's cache instead of being fetched again.
class PaintSnapshot final : public CefDevToolsMessageObserver {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  PaintSnapshot(CefRefPtr<CefBrowser> browser,Emit emit):browser_(browser),emit_(std::move(emit)) {}
  void Attach() { registration_=browser_->GetHost()->AddDevToolsMessageObserver(this); }
  void Detach() { registration_=nullptr;browser_=nullptr;calls_.clear(); }
  // {request_id, styles: [css property...]} -> paint_snapshot {request_id, ok, snapshot}
  void Capture(CefRefPtr<CefDictionaryValue> d) {
    auto params=CefDictionaryValue::Create();
    params->SetList("computedStyles",d->GetList("styles")->Copy());
    params->SetBool("includePaintOrder",true);params->SetBool("includeDOMRects",true);
    Call(Pending{Kind::Snapshot,d->GetInt("request_id"),{}},"DOMSnapshot.captureSnapshot",params);
  }
  // {request_id, url} -> paint_resource {request_id, ok, content, base64}
  void Resource(CefRefPtr<CefDictionaryValue> d) {
    Call(Pending{Kind::FrameTree,d->GetInt("request_id"),d->GetString("url").ToString()},"Page.getFrameTree",nullptr);
  }
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>,int id,bool success,const void* result,size_t size) override {
    // CEF may answer before ExecuteDevToolsMethod returns the id, and every
    // observer sees every result: copy, then match ids on the next UI turn.
    CefRefPtr<PaintSnapshot> self(this);
    std::string copy(static_cast<const char*>(result),size);
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PaintSnapshot> self,int id,bool success,std::string copy) {
      self->Result(id,success,copy);
    },self,id,success,std::move(copy)));
  }
  void OnDevToolsAgentDetached(CefRefPtr<CefBrowser>) override {
    auto calls=std::move(calls_);calls_.clear();
    for(auto& [id,pending]:calls) Fail(pending);
  }

 private:
  enum class Kind { Snapshot, FrameTree, Resource };
  struct Pending { Kind kind;int request;std::string url; };
  void Call(Pending pending,const char* method,CefRefPtr<CefDictionaryValue> params) {
    if(!browser_) return;
    const int id=browser_->GetHost()->ExecuteDevToolsMethod(0,method,params);
    if(!id) { Fail(pending);return; }
    calls_.emplace(id,std::move(pending));
  }
  void Result(int id,bool success,const std::string& json) {
    auto found=calls_.find(id);
    if(!browser_ || found==calls_.end()) return;
    Pending pending=std::move(found->second);calls_.erase(found);
    if(!success) { Fail(pending);return; }
    if(pending.kind==Kind::Snapshot) {
      auto fields=CefDictionaryValue::Create();fields->SetBool("ok",true);fields->SetString("snapshot",json);
      Reply("paint_snapshot",pending.request,fields);return;
    }
    auto parsed=CefParseJSON(json,JSON_PARSER_RFC);
    auto result=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
    if(pending.kind==Kind::FrameTree) {
      auto tree=result?result->GetDictionary("frameTree"):nullptr;
      auto frame=tree?tree->GetDictionary("frame"):nullptr;
      if(!frame || frame->GetType("id")!=VTYPE_STRING) { Fail(pending);return; }
      auto params=CefDictionaryValue::Create();
      params->SetString("frameId",frame->GetString("id"));params->SetString("url",pending.url);
      pending.kind=Kind::Resource;Call(std::move(pending),"Page.getResourceContent",params);return;
    }
    if(!result || result->GetType("content")!=VTYPE_STRING) { Fail(pending);return; }
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",true);
    fields->SetString("content",result->GetString("content"));
    fields->SetBool("base64",result->GetType("base64Encoded")==VTYPE_BOOL && result->GetBool("base64Encoded"));
    Reply("paint_resource",pending.request,fields);
  }
  void Fail(const Pending& pending) {
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",false);
    Reply(pending.kind==Kind::Snapshot?"paint_snapshot":"paint_resource",pending.request,fields);
  }
  void Reply(const char* event,int request,CefRefPtr<CefDictionaryValue> fields) {
    fields->SetInt("request_id",request);emit_(event,fields);
  }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;Emit emit_;
  std::map<int,Pending> calls_;
  IMPLEMENT_REFCOUNTING(PaintSnapshot);
};
}
