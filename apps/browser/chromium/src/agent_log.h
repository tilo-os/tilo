#pragma once
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <algorithm>
#include <cstdint>
#include <deque>
#include <functional>
#include <map>
#include <string>
#include <utility>

namespace tilo {
// What the page logged and fetched, for the agent operating the tab: console
// messages, uncaught errors and network requests, read from DevTools in the
// browser process. Nothing flows back into the page, and a request keeps its
// method, URL, type, status, size and failure only: never headers, bodies or
// cookies. Logging starts the first time an agent touches the tab, so a tab
// nobody automates pays nothing for it.
class AgentLog final : public CefDevToolsMessageObserver {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  AgentLog(CefRefPtr<CefBrowser> browser,Emit emit):browser_(browser),emit_(std::move(emit)) {}
  void Attach() { registration_=browser_->GetHost()->AddDevToolsMessageObserver(this); }
  void Detach() { registration_=nullptr;browser_=nullptr; }
  // Whether it records: the inspector reports the page's contexts.
  bool active() const { return active_; }
  // An agent is about to operate the tab: start recording. A page that was
  // already loaded is not fetched again, so its documents and frames are
  // backfilled from the frame tree: without them an agent cannot even name the
  // frame it needs to operate.
  void Activate() {
    if(active_ || !browser_) return;
    active_=true;
    auto host=browser_->GetHost();
    host->ExecuteDevToolsMethod(0,"Page.getFrameTree",nullptr);
    host->ExecuteDevToolsMethod(0,"Runtime.enable",nullptr);
    host->ExecuteDevToolsMethod(0,"Log.enable",nullptr);
    auto network=CefDictionaryValue::Create();
    // Bodies are never read, so the inspector need not keep them.
    network->SetInt("maxTotalBufferSize",0);network->SetInt("maxResourceBufferSize",0);
    host->ExecuteDevToolsMethod(0,"Network.enable",network);
  }
  // {request_id, action: console|network, since} -> page_result with data
  void Request(CefRefPtr<CefDictionaryValue> d) {
    const bool console=d->GetString("action")=="console";
    const double since=d->GetType("since")==VTYPE_DOUBLE?d->GetDouble("since"):d->GetInt("since");
    const bool started=!active_;
    Activate();
    const auto& entries=console?console_:network_;
    auto list=CefListValue::Create();size_t count=0;
    for(const auto& [seq,entry]:entries) {
      if(static_cast<double>(seq)<=since) continue;
      auto copy=entry->Copy(false);copy->SetDouble("seq",static_cast<double>(seq));
      list->SetDictionary(count++,copy);
    }
    auto data=CefDictionaryValue::Create();
    data->SetList("entries",list);data->SetDouble("next",static_cast<double>(seq_));
    data->SetDouble("dropped",static_cast<double>(console?console_dropped_:network_dropped_));
    data->SetString("provenance","web-content");
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",true);
    fields->SetString("url",browser_?browser_->GetMainFrame()->GetURL():CefString());
    std::string message=std::to_string(count)+(console?" console entries":" requests");
    if(started) message+="; recording started now, so earlier activity is not included";
    fields->SetString("message",message);
    fields->SetString("navigation","none");fields->SetBool("navigated",false);fields->SetBool("loading",false);fields->SetNull("outline");
    fields->SetDictionary("data",data);fields->SetInt("request_id",d->GetInt("request_id"));
    emit_("page_result",fields);
  }
  // A main-frame navigation began, with or without a person's (or an
  // action's) user activation behind it, as Chromium attributes it. Its
  // document request is then listed as that navigation.
  void MainNavigation(const std::string& url,bool user_gesture) {
    if(!active_) return;
    // The inspector lists a request's URL without its fragment.
    navigations_.emplace_back(url.substr(0,url.find('#')),user_gesture);
    if(navigations_.size()>kMaxNavigations) navigations_.pop_front();
  }
  void OnDevToolsEvent(CefRefPtr<CefBrowser>,const CefString& method,const void* params,size_t size) override {
    if(!active_) return;
    const std::string name=method.ToString();
    if(name!="Runtime.consoleAPICalled" && name!="Runtime.exceptionThrown" && name!="Log.entryAdded" &&
       name!="Page.frameNavigated" && name.rfind("Network.",0)!=0) return;
    auto parsed=CefParseJSON(std::string(static_cast<const char*>(params),size),JSON_PARSER_RFC);
    auto p=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
    if(!p) return;
    if(name=="Page.frameNavigated") { Frame(p);return; }
    if(name=="Runtime.consoleAPICalled") Console(p);
    else if(name=="Runtime.exceptionThrown") Exception(p);
    else if(name=="Log.entryAdded") LogEntry(p);
    else Network(name,p);
  }
  // The page's inspector went away: whatever is read next records afresh.
  void OnDevToolsAgentDetached(CefRefPtr<CefBrowser>) override { active_=false;navigations_.clear(); }

 private:
  static constexpr size_t kMaxEntries=500;
  static constexpr size_t kMaxText=500;
  static constexpr size_t kMaxUrl=500;
  static constexpr size_t kMaxNavigations=8;
  static std::string Cut(const std::string& text,size_t limit) {
    return text.size()>limit?text.substr(0,limit)+"…":text;
  }
  static std::string Str(CefRefPtr<CefDictionaryValue> d,const char* key) {
    return d && d->GetType(key)==VTYPE_STRING?d->GetString(key).ToString():std::string();
  }
  static double Num(CefRefPtr<CefDictionaryValue> d,const char* key) {
    if(!d) return 0;
    const auto type=d->GetType(key);
    return type==VTYPE_DOUBLE?d->GetDouble(key):type==VTYPE_INT?d->GetInt(key):0;
  }
  static CefRefPtr<CefDictionaryValue> Entry(const std::string& level,const std::string& text) {
    auto e=CefDictionaryValue::Create();e->SetString("level",level);e->SetString("text",Cut(text,kMaxText));
    return e;
  }
  static void Where(CefRefPtr<CefDictionaryValue> e,const std::string& url,double line) {
    if(url.empty()) return;
    e->SetString("source",Cut(url,kMaxUrl)+":"+std::to_string(static_cast<int64_t>(line)+1));
  }
  void Push(std::deque<std::pair<uint64_t,CefRefPtr<CefDictionaryValue>>>& to,size_t& dropped,CefRefPtr<CefDictionaryValue> e) {
    to.emplace_back(++seq_,e);
    while(to.size()>kMaxEntries) { to.pop_front();++dropped; }
  }
  // A document the page holds bytes for: its address, and its type as the
  // network log spells it, so frames read as documents beside top-level ones.
  void Frame(CefRefPtr<CefDictionaryValue> p) {
    auto frame=p->GetDictionary("frame");if(!frame) return;
    const auto url=Str(frame,"url");
    if(url.empty() || url.rfind("http",0)!=0) return;
    // The same address is one document however many frames hold it.
    for(const auto& [seq,entry]:network_)
      if(Str(entry,"type")=="Document" && Str(entry,"url")==url) return;
    auto e=CefDictionaryValue::Create();
    e->SetString("method","GET");e->SetString("url",Cut(url,kMaxUrl));
    e->SetString("type","Document");e->SetString("state","done");
    Push(network_,network_dropped_,e);
  }
  void Console(CefRefPtr<CefDictionaryValue> p) {
    std::string text;
    if(auto args=p->GetList("args")) for(size_t i=0;i<args->GetSize() && text.size()<kMaxText;++i) {
      auto arg=args->GetDictionary(i);if(!arg) continue;
      const auto type=arg->GetType("value");
      std::string part=type==VTYPE_STRING?arg->GetString("value").ToString()
        :type==VTYPE_DOUBLE||type==VTYPE_INT||type==VTYPE_BOOL?CefWriteJSON(arg->GetValue("value"),JSON_WRITER_DEFAULT).ToString()
        :Str(arg,"description").empty()?Str(arg,"type"):Str(arg,"description");
      text+=(text.empty()?"":" ")+part;
    }
    auto e=Entry(Str(p,"type"),text);
    if(auto stack=p->GetDictionary("stackTrace")) if(auto frames=stack->GetList("callFrames")) if(frames->GetSize()) {
      auto top=frames->GetDictionary(0);Where(e,Str(top,"url"),Num(top,"lineNumber"));
    }
    Push(console_,console_dropped_,e);
  }
  void Exception(CefRefPtr<CefDictionaryValue> p) {
    auto details=p->GetDictionary("exceptionDetails");if(!details) return;
    auto exception=details->GetDictionary("exception");
    const auto description=Str(exception,"description");
    auto e=Entry("error",description.empty()?Str(details,"text"):description);
    e->SetBool("uncaught",true);Where(e,Str(details,"url"),Num(details,"lineNumber"));
    Push(console_,console_dropped_,e);
  }
  void LogEntry(CefRefPtr<CefDictionaryValue> p) {
    auto entry=p->GetDictionary("entry");if(!entry) return;
    auto e=Entry(Str(entry,"level"),Str(entry,"text"));e->SetString("origin",Str(entry,"source"));
    Where(e,Str(entry,"url"),Num(entry,"lineNumber"));
    Push(console_,console_dropped_,e);
  }
  void Network(const std::string& name,CefRefPtr<CefDictionaryValue> p) {
    const auto id=Str(p,"requestId");if(id.empty()) return;
    if(name=="Network.requestWillBeSent") {
      auto request=p->GetDictionary("request");
      auto e=CefDictionaryValue::Create();
      e->SetString("method",Str(request,"method"));e->SetString("url",Cut(Str(request,"url"),kMaxUrl));
      e->SetString("type",Str(p,"type"));e->SetString("state","pending");
      if(Str(p,"type")=="Document") {
        const auto url=Str(request,"url");
        const auto found=std::find_if(navigations_.begin(),navigations_.end(),[&](const auto& n){return n.first==url;});
        if(found!=navigations_.end()) { e->SetBool("navigation",true);e->SetBool("user_gesture",found->second);navigations_.erase(found); }
      }
      started_[id]=Num(p,"timestamp");
      Push(network_,network_dropped_,e);open_[id]=e;
      if(open_.size()>kMaxEntries) { open_.erase(open_.begin());started_.erase(started_.begin()); }
      return;
    }
    auto found=open_.find(id);if(found==open_.end()) return;
    auto e=found->second;
    if(name=="Network.responseReceived") {
      auto response=p->GetDictionary("response");
      e->SetInt("status",static_cast<int>(Num(response,"status")));e->SetString("mime",Str(response,"mimeType"));
      return;
    }
    const auto start=started_.find(id);
    const double ms=start==started_.end()?0:(Num(p,"timestamp")-start->second)*1000;
    if(name=="Network.loadingFinished") {
      e->SetString("state","done");e->SetDouble("bytes",Num(p,"encodedDataLength"));e->SetInt("ms",static_cast<int>(ms));
    } else if(name=="Network.loadingFailed") {
      e->SetString("state","failed");e->SetString("error",Cut(Str(p,"errorText"),kMaxText));
      if(p->GetType("blockedReason")==VTYPE_STRING) e->SetString("blocked",Str(p,"blockedReason"));
      e->SetInt("ms",static_cast<int>(ms));
    } else return;
    open_.erase(found);if(start!=started_.end()) started_.erase(start);
  }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;Emit emit_;
  bool active_=false;uint64_t seq_=0;
  std::deque<std::pair<uint64_t,CefRefPtr<CefDictionaryValue>>> console_,network_;
  size_t console_dropped_=0,network_dropped_=0;
  std::map<std::string,CefRefPtr<CefDictionaryValue>> open_;
  std::map<std::string,double> started_;
  std::deque<std::pair<std::string,bool>> navigations_;
  IMPLEMENT_REFCOUNTING(AgentLog);
};
}
