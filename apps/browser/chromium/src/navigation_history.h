#pragma once
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include <functional>
#include <set>
#include <string>
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"

namespace tilo {
// The engine opens every tab on a blank page before its first address. That
// entry, first in the session history, is no page the person visited, so it
// is never a step back.
inline bool BootstrapEntry(int index,const std::string& url) { return index==0 && url=="about:blank"; }
// Whether all that is behind the current page is that blank page.
inline bool OnlyBootstrapBehind(CefRefPtr<CefBrowser> browser) {
  class Shape final : public CefNavigationEntryVisitor {
   public:
    bool Visit(CefRefPtr<CefNavigationEntry> entry,bool current,int index,int) override {
      if(index==0) bootstrap=BootstrapEntry(index,entry->GetURL().ToString());
      if(current) this->current=index;
      return !current && index<1;
    }
    bool bootstrap=false;int current=-1;
    IMPLEMENT_REFCOUNTING(Shape);
  };
  CefRefPtr<Shape> shape=new Shape;browser->GetHost()->GetNavigationEntries(shape,false);
  return shape->bootstrap && shape->current==1;
}
// The first page a tab opens takes the blank page's place: once it has
// committed, the session history forgets the blank page, so neither the page's
// own history.back() nor history.length can reach it. Called as the page
// commits, before its scripts can ask the browser to go back.
inline void ForgetBootstrap(CefRefPtr<CefBrowser> browser) {
  class Shape final : public CefNavigationEntryVisitor {
   public:
    bool Visit(CefRefPtr<CefNavigationEntry> entry,bool current,int index,int total) override {
      if(index==0) bootstrap=BootstrapEntry(index,entry->GetURL().ToString());
      if(current) this->current=index;
      this->total=total;
      return true;
    }
    bool bootstrap=false;int current=-1,total=0;
    IMPLEMENT_REFCOUNTING(Shape);
  };
  CefRefPtr<Shape> shape=new Shape;browser->GetHost()->GetNavigationEntries(shape,false);
  // Only the blank page and the page now open: nothing a person visited goes.
  if(shape->bootstrap && shape->current==1 && shape->total==2)
    browser->GetHost()->ExecuteDevToolsMethod(0,"Page.resetNavigationHistory",nullptr);
}
// Browser-process-only session history. Page content never receives this API.
class NavigationHistory final : public CefDevToolsMessageObserver {
 public:
  using Emit = std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  NavigationHistory(CefRefPtr<CefBrowser> browser,Emit emit):browser_(browser),emit_(std::move(emit)) {}
  void Attach() { registration_=browser_->GetHost()->AddDevToolsMessageObserver(this); }
  void Detach() { execution_refused_={};execution_pending_=0;registration_=nullptr;browser_=nullptr; }
  void Invalidate() { allowed_.clear();snapshot_=0;pending_=0; }
  void Request(int request,bool forward=false) {
    Invalidate();request_=request;forward_=forward;
    pending_=browser_->GetHost()->ExecuteDevToolsMethod(0,"Page.getNavigationHistory",nullptr);
    if(!pending_) Publish(nullptr);
  }
  bool Navigate(int request,int entry,std::function<void()> refused={}) {
    if(request!=snapshot_ || !allowed_.count(entry)) return false;
    auto params=CefDictionaryValue::Create();params->SetInt("entryId",entry);
    Invalidate();execution_refused_=std::move(refused);execution_pending_=browser_->GetHost()->ExecuteDevToolsMethod(0,"Page.navigateToHistoryEntry",params);return execution_pending_!=0;
  }
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>,int id,bool success,const void* result,size_t size) override {
    // CEF may answer synchronously, before ExecuteDevToolsMethod returns its id.
    // Copy callback-owned bytes and process on the next UI turn.
    CefRefPtr<NavigationHistory> self(this);
    std::string copy(static_cast<const char*>(result),size);
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<NavigationHistory> self,int id,bool success,std::string copy) {
      self->Result(id,success,copy);
    },self,id,success,std::move(copy)));
  }
 private:
  void Result(int id,bool success,const std::string& result) {
    if(!browser_)return;
    if(id==execution_pending_) {execution_pending_=0;auto refused=std::move(execution_refused_);if(!success && refused)refused();return;}
    if(id!=pending_) return;
    pending_=0;
    auto parsed=success?CefParseJSON(result,JSON_PARSER_RFC):nullptr;
    Publish(parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr);
    // Never execute a nested DevTools method in its result callback.
  }
  void Publish(CefRefPtr<CefDictionaryValue> result) {
    auto entries=CefListValue::Create();allowed_.clear();snapshot_=request_;
    if(result && result->GetType("currentIndex")==VTYPE_INT && result->GetType("entries")==VTYPE_LIST) {
      auto all=result->GetList("entries");int current=result->GetInt("currentIndex");
      if(current>=0 && size_t(current)<all->GetSize()) for(int i=forward_?current+1:current-1;i>=0 && size_t(i)<all->GetSize() && entries->GetSize()<12;i+=forward_?1:-1) {
        auto entry=all->GetDictionary(i);
        if(!entry || entry->GetType("id")!=VTYPE_INT || entry->GetInt("id")<=0 || entry->GetType("url")!=VTYPE_STRING || entry->GetType("title")!=VTYPE_STRING) continue;
        if(entry->GetString("url").ToString().size()>8192 || entry->GetString("title").ToString().size()>4096) continue;
        if(BootstrapEntry(i,entry->GetString("url").ToString())) continue;
        auto row=CefDictionaryValue::Create();int id=entry->GetInt("id");
        row->SetInt("id",id);row->SetString("url",entry->GetString("url"));row->SetString("title",entry->GetString("title"));
        allowed_.insert(id);entries->SetDictionary(entries->GetSize(),row);
      }
    }
    auto event=CefDictionaryValue::Create();event->SetInt("request_id",request_);event->SetString("direction",forward_?"forward":"back");event->SetList("entries",entries);emit_("navigation_history",event);
  }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;Emit emit_;
  std::function<void()> execution_refused_;
  int execution_pending_=0;
  int pending_=0,request_=0,snapshot_=0;bool forward_=false;std::set<int> allowed_;
  IMPLEMENT_REFCOUNTING(NavigationHistory);
};
}
