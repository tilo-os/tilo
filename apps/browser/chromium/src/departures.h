#pragma once
// What a page is ready to send, held until the person allows it.
//
// A window an agent operates holds every departure: a form submission however
// it was triggered, a navigation or request with a method of its own, and a
// download. The request is paused where Chromium hands it over — nothing
// leaves, the page stays as it was filled — and the app answers it by its id:
// allow continues it exactly once, and anything else drops it. A person's own
// window never holds anything, so this is only ever used by a window opened
// for an agent (docs/hosted-browser.md).
//
// The tab's record says what was held and how it ended (HoldNote): a reviewer
// reads that a submission waited for the person, who let it leave, or that the
// page was kept as it is.
#include "include/base/cef_callback.h"
#include "include/cef_browser.h"
#include "include/cef_callback.h"
#include "include/cef_frame.h"
#include "include/cef_request.h"
#include "include/cef_resource_request_handler.h"
#include "include/cef_values.h"
#include "include/wrapper/cef_closure_task.h"
#include "site_policy.h"
#include <algorithm>
#include <chrono>
#include <functional>
#include <map>
#include <memory>
#include <string>
#include <utility>
#include <vector>

namespace tilo {
// The methods that leave with data of their own: a GET or HEAD request only
// asks for a page, and anything else sends something.
inline bool LeavesWithData(const std::string& method) {
  return method!="GET" && method!="HEAD" && method!="OPTIONS";
}
// Whether a navigation is a form leaving: Chromium marks the page transition
// of a form a person sent, and a form the page's own script sends carries the
// data it sends. A navigation that does neither only asks for a page.
inline bool Submission(const CefRefPtr<CefRequest>& request) {
  return (request->GetTransitionType() & TT_SOURCE_MASK)==TT_FORM_SUBMIT ||
         LeavesWithData(request->GetMethod().ToString());
}
// Whether `url` is a web address, the only kind held: an extension's page, a
// data: URL or the engine's own pages reach nothing.
inline bool WebAddress(const std::string& url) {
  return url.compare(0,7,"http://")==0 || url.compare(0,8,"https://")==0;
}
// What the tab's record is told of a hold (PageAutomation::HoldBegan and
// HoldEnded): that something the page was about to send waits for the person,
// or how that ended. It names what the person is asked about, its method and
// where it would go, and of a download the file it would save, and never what
// it carries: no request's body and nothing a form's fields hold.
struct HoldNote {
  // What the record names it by: the kind of what was held (`submit`,
  // `request`, `download`), or how a held one ended (`allow`, `keep`, `drop`).
  std::string action;
  // Whether it says that a hold began.
  bool began=false;
  // Who ended it, as the app says: the name the tab keeps that actor's
  // actions under, and nobody's for the person at the window and for a hold
  // nobody answered. A hold that begins is the running action's actor's.
  std::string actor;
  std::string method,origin,file;
};
using NoteHold=std::function<void(int,const HoldNote&)>;
// How an answer ends a hold, as the record names it: let leave, kept by
// whoever answered, or dropped because nobody did.
inline const char* HoldEnding(bool allow,bool expired) { return allow?"allow":expired?"drop":"keep"; }
// One request held before it leaves.
struct Departure {
  int tab=0;
  // "submit" for a form leaving, "request" for a fetch or XHR.
  const char* kind="request";
  std::string method;
  std::string origin;
  // A request paused in OnBeforeResourceLoad: continued on allow, cancelled
  // by letting the callback go.
  CefRefPtr<CefCallback> load;
  // A navigation Chromium was told not to start: issued again on allow.
  CefRefPtr<CefFrame> frame;
  CefRefPtr<CefRequest> request;
};

// Every request this engine holds, by the id the app answers it with. Only
// Chromium's UI thread touches it: the handlers that hold and every command
// that answers run there.
class Departures final {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  explicit Departures(Emit emit,NoteHold note=nullptr):emit_(std::move(emit)),note_(std::move(note)) {}
  // Hold `departure` under a fresh id, telling the app what is about to leave
  // and where. Returns the id the answer names.
  int Hold(Departure departure) {
    const int id=next_++;
    auto fields=CefDictionaryValue::Create();
    fields->SetInt("tab",departure.tab);fields->SetInt("id",id);
    fields->SetString("kind",departure.kind);
    fields->SetString("method",departure.method);
    fields->SetString("origin",departure.origin);
    emit_("held",fields);
    Note(departure,departure.kind,true,{});
    held_[id]=std::move(departure);
    return id;
  }
  // Whether a navigation about to start is one the person already allowed,
  // issued again from the answer: it leaves without asking a second time. An
  // allowance is spent by the request it was made for, and lapses soon after
  // in case that request never comes.
  bool TakesAllowance(int tab,const std::string& method,const std::string& url) {
    const auto now=std::chrono::steady_clock::now();
    for(auto entry=allowed_.begin();entry!=allowed_.end();++entry) {
      if(now>=entry->until) continue;
      if(entry->tab!=tab || entry->method!=method || entry->url!=url) continue;
      allowed_.erase(entry);
      return true;
    }
    return false;
  }
  // The app answered `id`: let it leave exactly once when `allow`, and drop
  // it otherwise. An id that is not held is a command that came too late.
  // `actor` is who answered, as the app says, and `expired` that nobody did
  // and the app dropped it at its own deadline: the tab's record says which.
  // An app that says neither answers as the person at the window.
  void Answer(int id,bool allow,const std::string& actor={},bool expired=false) {
    auto found=held_.find(id);
    if(found==held_.end()) return;
    Departure departure=std::move(found->second);
    held_.erase(found);
    Note(departure,HoldEnding(allow,expired),false,actor);
    Closed(departure.tab,id);
    // Not allowed: a paused load is cancelled, and a navigation never started.
    if(!allow) { if(departure.load) departure.load->Cancel(); return; }
    if(departure.load) {departure.load->Continue();return;}
    if(departure.frame && departure.request) {
      allowed_.push_back(Allowance{departure.tab,departure.method,
        departure.request->GetURL().ToString(),
        std::chrono::steady_clock::now()+std::chrono::minutes(1)});
      departure.frame->LoadRequest(departure.request);
    }
  }
  // The tab closed or navigated away from what it held: nothing of it leaves,
  // and its requests are dropped.
  void Closed(int tab) {
    std::vector<int> ids;
    for(const auto& entry:held_) if(entry.second.tab==tab) ids.push_back(entry.first);
    for(int id:ids) {
      auto found=held_.find(id);
      if(found==held_.end()) continue;
      Departure departure=std::move(found->second);
      held_.erase(found);
      Note(departure,HoldEnding(false,true),false,{});
      Closed(tab,id);
      if(departure.load) departure.load->Cancel();
    }
  }
 private:
  void Note(const Departure& departure,const char* action,bool began,const std::string& actor) {
    if(!note_) return;
    note_(departure.tab,HoldNote{action,began,actor,departure.method,departure.origin,{}});
  }
  void Closed(int tab,int id) {
    auto fields=CefDictionaryValue::Create();
    fields->SetInt("tab",tab);fields->SetInt("id",id);
    emit_("held_closed",fields);
  }
  Emit emit_;
  NoteHold note_;
  std::map<int,Departure> held_;
  // A navigation the person allowed, issued again, and not yet seen.
  struct Allowance {int tab;std::string method,url;std::chrono::steady_clock::time_point until;};
  std::vector<Allowance> allowed_;
  int next_=1;
};

// The resource handler a held window gives a request of the page's own that
// leaves with data (a fetch or an XHR): it pauses it where Chromium lets the
// page's load go, until the person answers it.
class HoldResourceHandler final : public CefResourceRequestHandler {
 public:
  HoldResourceHandler(int tab,std::shared_ptr<Departures> departures):tab_(tab),departures_(std::move(departures)) {}
  // Chromium calls this on its IO thread and the ledger is the UI thread's
  // (see Departures), so the hold is recorded there: the load stays paused
  // until that hold is answered.
  cef_return_value_t OnBeforeResourceLoad(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>,
      CefRefPtr<CefRequest> request,CefRefPtr<CefCallback> callback) override {
    if(!departures_) return RV_CONTINUE;
    CefPostTask(TID_UI,base::BindOnce([](std::shared_ptr<Departures> departures,int tab,
        std::string method,std::string url,CefRefPtr<CefCallback> load) {
      Departure departure;
      departure.tab=tab;departure.kind="request";departure.method=std::move(method);
      departure.origin=Origin(url);departure.load=std::move(load);
      departures->Hold(std::move(departure));
    },departures_,tab_,request->GetMethod().ToString(),request->GetURL().ToString(),callback));
    return RV_CONTINUE_ASYNC;
  }
  IMPLEMENT_REFCOUNTING(HoldResourceHandler);
 private:
  int tab_;
  std::shared_ptr<Departures> departures_;
};
}
