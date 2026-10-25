#pragma once
#include "page_automation_script.h"
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <algorithm>
#include <functional>
#include <chrono>
#include <optional>
#include <string>
#include <utility>
#include <vector>

namespace tilo {
// Documents are numbered across every tab of this host, so a ref read in one
// tab never resolves in another.
inline int page_documents=0;
// The frame part of a ref, as the page writes it: `f<digits>`, bounded. Frames
// come from the browser's own frame tree, never from the page, so this is only
// how an action says which frame its ref was read in.
inline std::optional<int> FrameOfRef(const std::string& ref) {
  if(ref.size()<4 || ref[0]!='f') return std::nullopt;
  int frame=0;
  for(size_t i=1;i<ref.size();++i) {
    const char digit=ref[i];
    if(digit=='d') break;
    if(digit<'0' || digit>'9' || i>8) return std::nullopt;
    frame=frame*10+(digit-'0');
  }
  return frame;
}

// Browser-process page reading and operation for the browser's page actions.
// Script runs in an isolated world of the main frame, created through
// DevTools: it shares the page's DOM, but page script can neither see nor
// call it. Clicks and keys go through the same CEF input path as a person's,
// so the page receives trusted events with user activation.
class PageAutomation final : public CefDevToolsMessageObserver {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  // CSS pixels in the main frame's viewport.
  using Click=std::function<void(double,double)>;
  using Key=std::function<void(CefRefPtr<CefDictionaryValue>)>;
  PageAutomation(CefRefPtr<CefBrowser> browser,Emit emit,Click click,Key key)
      :browser_(browser),emit_(std::move(emit)),click_(std::move(click)),key_(std::move(key)) {}
  void Attach() { registration_=browser_->GetHost()->AddDevToolsMessageObserver(this); }
  void Detach() { registration_=nullptr;browser_=nullptr;pending_.reset(); }
  // A main-frame navigation began. An action that caused one answers once it
  // loads, so the caller's next read sees the page the action opened.
  void NavigationStarted() {
    ++navigations_;
    if(pending_ && pending_->awaiting_start) { pending_->awaiting_start=false;pending_->awaiting_load=true; }
  }
  void NavigationFinished() {
    if(pending_ && pending_->awaiting_load) Succeed(pending_->message,std::nullopt);
  }
  void Request(CefRefPtr<CefDictionaryValue> command) {
    const int request=command->GetInt("request_id");
    if(!browser_) return;
    if(pending_) { Reply(request,Failure("busy","Another page action is still running in this tab"),command->GetString("op")=="text_input");return; }
    const bool private_text=command->GetString("op")=="text_input";
    auto owned=command->Copy(false);
    if(private_text) {
      const double now=std::chrono::duration<double,std::milli>(std::chrono::system_clock::now().time_since_epoch()).count();
      if(!owned->HasKey("expires_ms")) owned->SetDouble("expires_ms",now+5000);
      if(Number(owned,"expires_ms")<=now || Number(owned,"expires_ms")>now+10000) {
        Reply(request,Failure("text_input_expired","The original text request expired"),true);return;
      }
    }
    native_attempted_=false;native_object_.clear();
    pending_=Pending{request,++token_,navigations_,false,owned};
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->Fail("timeout","The page did not answer in time");
    },CefRefPtr<PageAutomation>(this),token_),private_text?5000:kTimeoutMs);
    Begin();
  }
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>,int id,bool success,const void* result,size_t size) override {
    // CEF may answer synchronously, before ExecuteDevToolsMethod returns its id.
    // Copy callback-owned bytes and process on the next UI turn.
    CefRefPtr<PageAutomation> self(this);
    std::string copy(static_cast<const char*>(result),size);
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,int id,bool success,std::string copy) {
      self->Result(id,success,copy);
    },self,id,success,std::move(copy)));
  }
  void OnDevToolsAgentDetached(CefRefPtr<CefBrowser>) override {
    world_=0;loader_.clear();text_owner_.reset();text_lease_.clear();
    if(pending_ && call_) Fail("unavailable","The page's inspector connection closed");
  }

 private:
  static constexpr int64_t kTimeoutMs=30000;
  // A started navigation is awaited this long; a slower one is reported as
  // still loading and the caller waits for it.
  static constexpr int64_t kLoadWaitMs=10000;
  enum class Stage { FrameTree, World, TextWorld, TextFocus, TextFailureFrameTree, NativeElement, NativeDOM, NativeDocument, NativeNode, NativeEditors, NativeResolve, NativeConfigure, FrameRead, FrameAnswer, FrameAction, FramePlace, Evaluate, Fence };
  struct TextFrame { std::string id,loader;int world=0,document=0;bool same_origin=true; };
  // One child frame of the document being read, in the order the page shows
  // it. Its own isolated world is created separately, so a frame of another
  // site is read exactly as a frame of this one is: nothing is shared with the
  // page and nothing of the page is loosened for the frame.
  struct Frame {
    std::string id,loader,url,title,key;size_t position=0;
    bool cross=false,read=false,refused=false;int world=0;
    std::string outline,message;
    // The frame's own view origin, so a point inside it can be placed on the
    // page by the document that shows it.
    double top=0,left=0;
    struct Edges { std::string top,left;bool hidden=false;bool readable=true; };
  };
  struct Pending {
    int request;uint64_t token;uint64_t navigations;bool retried;
    CefRefPtr<CefDictionaryValue> command;std::string message;
    // "none": the action ran no page code. "started": the page began leaving
    // the document while it handled the action's input. "possible": it had
    // not, and may still navigate later on its own.
    std::string navigation="none";bool awaiting_start=false,awaiting_load=false;
  };
  std::string Action() const { return pending_->command->GetString("action").ToString(); }
  bool PrivateText() const { return pending_->command->GetString("op")=="text_input"; }
  bool CollectTextFrames(CefRefPtr<CefDictionaryValue> tree,std::vector<TextFrame>& frames,const std::string& root_origin={},bool observed_ancestors=true) {
    if(!tree || frames.size()>=128)return false;
    auto frame=tree->GetDictionary("frame");
    if(!frame || frame->GetType("id")!=VTYPE_STRING || frame->GetType("loaderId")!=VTYPE_STRING || frame->GetType("securityOrigin")!=VTYPE_STRING)return false;
    const auto origin=frame->GetString("securityOrigin").ToString();
    const auto root=root_origin.empty()?origin:root_origin;
    const bool observed=observed_ancestors && origin==root;
    frames.push_back({frame->GetString("id").ToString(),frame->GetString("loaderId").ToString(),0,0,observed});
    auto children=tree->GetList("childFrames");
    if(children)for(size_t i=0;i<children->GetSize();++i)
      if(!CollectTextFrames(children->GetDictionary(i),frames,root,observed))return false;
    return true;
  }
  void ProbeTextFrame() {
    // Cross-origin ancestors cannot be observed without widening DOM authority.
    while(!text_candidates_.empty() && !text_candidates_.front().same_origin)text_candidates_.erase(text_candidates_.begin());
    if(text_candidates_.empty()) { Fail("unavailable","The focused frame has no isolated text context");return; }
    text_frame_=text_candidates_.front();text_candidates_.erase(text_candidates_.begin());
    auto params=CefDictionaryValue::Create();
    params->SetString("frameId",text_frame_.id);params->SetString("worldName","tilo-private-text-input");
    // No universal access, origin bypass or debugger pause is requested.
    Call(Stage::TextWorld,"Page.createIsolatedWorld",params);
  }
  void Begin() {
    if(Action()=="press" && !pending_->command->HasKey("element")) { Act(nullptr);return; }
    // A ref read inside a frame names the frame it came from; an action on one
    // runs there, and in every other world only the frames already read are
    // looked up, so the refs its caller holds keep resolving.
    frame_route_=FrameOfRef(pending_->command->GetString("element").ToString()).has_value();
    Call(Stage::FrameTree,"Page.getFrameTree",nullptr);
  }
  void Call(Stage stage,const char* method,CefRefPtr<CefDictionaryValue> params) {
    stage_=stage;call_=browser_->GetHost()->ExecuteDevToolsMethod(0,method,params);
    if(!call_) Fail("unavailable","The page's inspector connection is unavailable");
  }
  void Result(int id,bool success,const std::string& json) {
    if(!browser_ || !pending_ || !call_ || id!=call_) return;
    call_=0;
    auto parsed=CefParseJSON(json,JSON_PARSER_RFC);
    auto result=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
    // Never execute a nested DevTools method in its result callback; this
    // already runs on a later UI turn.
    if(stage_==Stage::NativeElement || stage_==Stage::NativeDOM || stage_==Stage::NativeDocument ||
        stage_==Stage::NativeNode || stage_==Stage::NativeEditors || stage_==Stage::NativeResolve || stage_==Stage::NativeConfigure) {
      NativeResult(success,result);return;
    }
    if(stage_==Stage::TextFailureFrameTree) {
      std::vector<TextFrame> frames;
      auto tree=success && result?result->GetDictionary("frameTree"):nullptr;
      if(text_owner_ && CollectTextFrames(tree,frames)) {
        const auto owner=*text_owner_;
        const bool live=std::any_of(frames.begin(),frames.end(),[&](const TextFrame& candidate) {
          return candidate.id==owner.id && candidate.loader==owner.loader;
        });
        if(!live) { Fail("text_input_retired","The original checked frame was replaced");return; }
      }
      Fail("unavailable","The original text request was not acknowledged");return;
    }
    if(stage_==Stage::FrameTree) {
      auto tree=success && result?result->GetDictionary("frameTree"):nullptr;
      // An action on a ref inside a frame runs there, in that frame's own
      // world. It needs no world of the page's own, so nothing of the page is
      // read or touched for it.
      if(frame_route_ && !PrivateText()) {
        if(tree) CollectFrames(tree,0);
        FrameAct();return;
      }
      auto frame=tree?tree->GetDictionary("frame"):nullptr;
      if(!frame || frame->GetType("id")!=VTYPE_STRING || frame->GetType("loaderId")!=VTYPE_STRING) {
        Fail("unavailable","The page has no document to read");return;
      }
      if(PrivateText()) {
        std::vector<TextFrame> frames;
        if(!CollectTextFrames(tree,frames)) { Fail("unavailable","The frame tree cannot be bounded and observed");return; }
        if(Action()=="prepare_text_input") {
          text_candidates_=std::move(frames);ProbeTextFrame();return;
        }
        const auto lease=pending_->command->GetString("lease").ToString();
        if(!text_owner_ || lease!=text_lease_) { Fail("text_input_retired","The original frame lease is no longer active");return; }
        const auto owner=*text_owner_;
        const bool live=std::any_of(frames.begin(),frames.end(),[&](const TextFrame& candidate) {
          return candidate.id==owner.id && candidate.loader==owner.loader;
        });
        if(!live) { text_owner_.reset();text_lease_.clear();Fail("text_input_retired","The original frame document was replaced");return; }
        text_frame_=owner;Evaluate();return;
      }
      const auto loader=frame->GetString("loaderId").ToString();
      if(world_ && loader==loader_) { AfterWorld();return; }
      loader_=loader;world_=0;
      auto params=CefDictionaryValue::Create();
      params->SetString("frameId",frame->GetString("id"));params->SetString("worldName","tilo-page-actions");
      Call(Stage::World,"Page.createIsolatedWorld",params);
    } else if(stage_==Stage::TextWorld) {
      if(!success || !result || result->GetType("executionContextId")!=VTYPE_INT) { ProbeTextFrame();return; }
      text_frame_.world=result->GetInt("executionContextId");
      text_frame_.document=page_documents=page_documents%2147483647+1;
      auto params=CefDictionaryValue::Create();
      // Observe focus only. No transcript or field content is extracted here.
      params->SetString("expression","document.hasFocus() && (()=>{let e=document.activeElement;while(e?.shadowRoot?.activeElement)e=e.shadowRoot.activeElement;return !!e && e.localName!=='iframe' && e.localName!=='frame';})()");
      params->SetInt("contextId",text_frame_.world);params->SetBool("returnByValue",true);params->SetBool("silent",true);
      Call(Stage::TextFocus,"Runtime.evaluate",params);
    } else if(stage_==Stage::TextFocus) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      if(!remote || remote->GetType("value")!=VTYPE_BOOL || !remote->GetBool("value")) { ProbeTextFrame();return; }
      Evaluate();
    } else if(stage_==Stage::World) {
      if(!success || !result || result->GetType("executionContextId")!=VTYPE_INT) {
        Fail("unavailable","The page refused a script context");return;
      }
      world_=result->GetInt("executionContextId");
      document_=page_documents=page_documents%2147483647+1;
      // This document's frames are the ones the tree names, now that the
      // document that shows them is the one just created.
      AfterWorld();
    } else if(stage_==Stage::FrameRead) {
      FrameValues(success,result);
    } else if(stage_==Stage::FrameAnswer) {
      FrameAnswered(success,result);
    } else if(stage_==Stage::FrameAction) {
      FrameActed(success,result);
    } else if(stage_==Stage::FramePlace) {
      FramePlaced(success,result);
    } else if(stage_==Stage::Fence) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
      // No answer means the document went away under the fence: a navigation
      // that began during the action has committed.
      Fenced(value?Text(value,"navigation"):navigations_!=pending_->navigations?"started":"possible");
    } else {
      if(!success) {
        if(PrivateText()) {
          // A failed non-editing check may observe retirement, never replay an
          // evaluation or retarget. An ambiguous commit stays unknown.
          if(Action()=="check_text_input" || Action()=="start_text_input" || Action()=="cancel_text_input") {
            Call(Stage::TextFailureFrameTree,"Page.getFrameTree",nullptr);return;
          }
          Fail("unavailable","The original text request was not acknowledged");return;
        }
        // The document changed under a cached context. Retry once in the new
        // document; a ref from the old one then fails as stale.
        world_=0;loader_.clear();frames_.clear();frames_at_=0;
        if(pending_->retried) { Fail("page_changed","The page changed while it was being read");return; }
        pending_->retried=true;Begin();return;
      }
      auto remote=result?result->GetDictionary("result"):nullptr;
      auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
      if(!value || value->GetType("ok")!=VTYPE_BOOL) { Fail("script","The page did not return a result");return; }
      if(!value->GetBool("ok")) {
        const auto code=value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"script";
        if(code=="text_input_native_required" && PrivateText() && Action()=="prepare_text_input" &&
            pending_->command->GetBool("native_caret") && !native_attempted_) {
          native_attempted_=true;
          auto params=CefDictionaryValue::Create();
          params->SetString("expression","globalThis.__tiloPage.nativeTextInputElement()");
          params->SetString("objectGroup","tilo-private-native-caret");
          params->SetInt("contextId",text_frame_.world);params->SetBool("silent",true);
          Call(Stage::NativeElement,"Runtime.evaluate",params);return;
        }
        Fail(code.size()<=64?code:"script",Text(value,"message"));return;
      }
      Act(value);
    }
  }
  // Resolve only node IDs and an editor object in the existing private world.
  // No field contents, attributes or caret offsets are exported to the client; no
  // remote-debugging port or universal-access world is opened.
  void NativeResult(bool success,CefRefPtr<CefDictionaryValue> result) {
    if(!success || !result) { Fail("not_editable","The original native caret cannot be observed");return; }
    auto params=CefDictionaryValue::Create();
    if(stage_==Stage::NativeElement) {
      auto remote=result->GetDictionary("result");
      if(!remote || remote->GetType("objectId")!=VTYPE_STRING) { Fail("not_editable","The original native field was lost");return; }
      native_object_=remote->GetString("objectId").ToString();
      Call(Stage::NativeDOM,"DOM.enable",nullptr);
    } else if(stage_==Stage::NativeDOM) {
      params->SetInt("depth",0);params->SetBool("pierce",false);
      Call(Stage::NativeDocument,"DOM.getDocument",params);
    } else if(stage_==Stage::NativeDocument) {
      params->SetString("objectId",native_object_);
      Call(Stage::NativeNode,"DOM.requestNode",params);
    } else if(stage_==Stage::NativeNode) {
      if(result->GetType("nodeId")!=VTYPE_INT || result->GetInt("nodeId")<=0) { Fail("not_editable","The original native field was lost");return; }
      params->SetInt("nodeId",result->GetInt("nodeId"));params->SetBool("pierce",true);
      auto styles=CefListValue::Create();auto style=CefDictionaryValue::Create();
      style->SetString("name","-webkit-user-modify");style->SetString("value","read-write-plaintext-only");
      styles->SetDictionary(0,style);params->SetList("computedStyles",styles);
      Call(Stage::NativeEditors,"DOM.getNodesForSubtreeByStyle",params);
    } else if(stage_==Stage::NativeEditors) {
      auto nodes=result->GetList("nodeIds");
      if(!nodes || nodes->GetSize()!=1 || nodes->GetType(0)!=VTYPE_INT) { Fail("not_editable","The native editor cannot be uniquely observed");return; }
      params->SetInt("nodeId",nodes->GetInt(0));params->SetInt("executionContextId",text_frame_.world);
      params->SetString("objectGroup","tilo-private-native-caret");
      Call(Stage::NativeResolve,"DOM.resolveNode",params);
    } else if(stage_==Stage::NativeResolve) {
      auto object=result->GetDictionary("object");
      if(!object || object->GetType("objectId")!=VTYPE_STRING) { Fail("not_editable","The original native editor was lost");return; }
      params->SetString("objectId",object->GetString("objectId"));
      params->SetString("functionDeclaration","function(){return globalThis.__tiloPage.configureNativeTextEditor(this)}");
      params->SetBool("returnByValue",true);params->SetBool("silent",true);
      Call(Stage::NativeConfigure,"Runtime.callFunctionOn",params);
    } else {
      auto remote=result->GetDictionary("result");
      if(!remote || remote->GetType("value")!=VTYPE_BOOL || !remote->GetBool("value")) { Fail("text_input_changed","The original native field changed");return; }
      Evaluate();
    }
  }
  // What follows this document's own read: a read of `outline` or `changes`
  // reads the frames it shows too, and every other action goes straight on.
  void AfterWorld() {
    // Every action reads the frames the page shows before it acts, so a ref
    // inside a frame resolves the same way on a click as on a read.
    if(frame_route_) { Evaluate();return; }
    // A read also lists what the frames show: each one answers with its own
    // lines, which the page's outline indents under it.
    reading_=Action()=="outline" || Action()=="changes";
    FrameNext();
  }
  // How many child frames one read descends into and how deep, so a page that
  // nests frames without end cannot make a read without end.
  static constexpr size_t kMaxFrames=16;
  static constexpr size_t kMaxFrameDepth=3;
  // Each frame's own outline is bounded below the page's, so sixteen frames
  // cannot bury the page's own lines.
  static constexpr int kFrameLines=200;
  // Every child frame this document shows, in the page's own order.
  void CollectFrames(CefRefPtr<CefDictionaryValue> tree,size_t depth) {
    if(!tree || depth>=kMaxFrameDepth) return;
    auto children=tree->GetList("childFrames");
    for(size_t i=0;children && i<children->GetSize() && frames_.size()<kMaxFrames;++i) {
      auto child=children->GetDictionary(i);
      auto own=child?child->GetDictionary("frame"):nullptr;
      if(!own || own->GetType("id")!=VTYPE_STRING || own->GetType("loaderId")!=VTYPE_STRING) continue;
      Frame found;
      found.id=own->GetString("id").ToString();found.loader=own->GetString("loaderId").ToString();
      found.key="f"+std::to_string(frames_.size()+1);
      frames_.push_back(found);
      CollectFrames(child,depth+1);
    }
  }
  // The next frame still unread, in its own isolated world. A frame the
  // browser refuses to give a world is listed as unread, never dropped.
  void FrameNext() {
    while(frames_at_<frames_.size() && frames_[frames_at_].read) ++frames_at_;
    if(frames_at_>=frames_.size()) { Evaluate();return; }
    auto params=CefDictionaryValue::Create();
    params->SetString("frameId",frames_[frames_at_].id);
    params->SetString("worldName","tilo-page-actions");
    // The world sees the frame's own document and nothing more.
    params->SetBool("grantUniversalAccess",false);
    Call(Stage::FrameRead,"Page.createIsolatedWorld",params);
  }
  void FrameSkipped(bool refused) {
    frames_[frames_at_].read=true;frames_[frames_at_].refused=refused;
    ++frames_at_;FrameNext();
  }
  // A frame's own read: its title, its own lines, and where its element sits
  // in the document that shows it, so a point inside it can be placed.
  void FrameValues(bool success,CefRefPtr<CefDictionaryValue> result) {
    if(!success || !result || result->GetType("executionContextId")!=VTYPE_INT) { FrameSkipped(true);return; }
    frames_[frames_at_].world=result->GetInt("executionContextId");
    auto command=CefDictionaryValue::Create();
    command->SetString("action","frame_outline");command->SetInt("doc",document_);
    command->SetInt("max_lines",kFrameLines);
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+
      CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",frames_[frames_at_].world);params->SetBool("returnByValue",true);
    // Reading a frame shows it nothing and grants it no user activation.
    params->SetBool("userGesture",false);params->SetBool("silent",true);params->SetBool("awaitPromise",true);
    Call(Stage::FrameAnswer,"Runtime.evaluate",params);
  }
  void FrameAnswered(bool success,CefRefPtr<CefDictionaryValue> result) {
    auto value=Value(success,result);
    auto& frame=frames_[frames_at_];
    if(!value || value->GetType("ok")!=VTYPE_BOOL || !value->GetBool("ok")) { FrameSkipped(false);return; }
    frame.title=Text(value,"title");frame.url=Text(value,"url");frame.outline=Text(value,"outline");
    frame.top=Number(value,"top");frame.left=Number(value,"left");
    frame.read=true;++frames_at_;FrameNext();
  }
  // `outline` and `changes` as the caller sees them: the page's own lines, and
  // every frame's lines indented under the line that names the frame.
  std::string Merged(CefRefPtr<CefDictionaryValue> value) const {
    const auto own=Text(value,"outline");
    if(frames_.empty()) return own;
    std::string out,line;
    size_t next=0;
    for(size_t i=0;i<=own.size();++i) {
      if(i<own.size() && own[i]!='\n') { line+=own[i];continue; }
      out+=line;out+='\n';line.clear();
      // A frame element's line is the only place a frame is shown, so the
      // frames that were read follow it in the page's order.
      if(next<frames_.size() && Shown(line)) out+=FrameLines(frames_[next++]);
    }
    while(!out.empty() && out.back()=='\n') out.pop_back();
    return out;
  }
  static bool Shown(const std::string& line) {
    return line.find("iframe \"")!=std::string::npos && line.find("[ref=")!=std::string::npos;
  }
  std::string FrameLines(const Frame& frame) const {
    if(frame.outline.empty()) {
      std::string out="    - text \"… this frame is not read";
      out+=frame.refused?": the page refused it\"":": read the page again\"";
      return out+"\n";
    }
    // The frame's lines were laid out from its own depth zero.
    std::string out;
    for(size_t begin=0;begin<frame.outline.size();) {
      auto end=frame.outline.find('\n',begin);
      if(end==std::string::npos) end=frame.outline.size();
      out+="  ";out+=frame.outline.substr(begin,end-begin);out+='\n';
      begin=end+1;
    }
    return out;
  }
  // Whether the caller's action names a ref inside a frame, and which frame.
  std::optional<size_t> FrameByRef() const {
    const auto frame=FrameOfRef(pending_->command->GetString("element").ToString());
    if(!frame) return std::nullopt;
    const auto key="f"+std::to_string(*frame);
    const auto found=std::find_if(frames_.begin(),frames_.end(),[&](const Frame& entry) { return entry.key==key; });
    return found==frames_.end()?frames_.size():
      static_cast<size_t>(std::distance(frames_.begin(),found));
  }
  // An action on a ref inside a frame runs in that frame's own world.
  void FrameAct() {
    const auto found=FrameByRef();
    if(!found) { Evaluate();return; }
    if(*found>=frames_.size() || !frames_[*found].world) {
      Fail("stale_element","That element is in a frame this tab has not read; read the page again");return;
    }
    frame_=*found;command_=Fields(Action());
    context_=frames_[*found].world;
    Call(Stage::FrameAction,"Runtime.evaluate",Params());
  }
  void FrameActed(bool success,CefRefPtr<CefDictionaryValue> result) {
    auto value=Value(success,result);
    if(!value || value->GetType("ok")!=VTYPE_BOOL) { Fail("script","The frame did not return a result");return; }
    if(!value->GetBool("ok")) {
      const auto code=value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"script";
      Fail(code.size()<=64?code:"script",Text(value,"message"));return;
    }
    // A click is at a point of the frame's own view; `Act` places it on the
    // page, exactly as it places a click in the page's own document.
    frame_message_=Text(value,"message");
    Act(value);
  }
  void FramePlaced(bool success,CefRefPtr<CefDictionaryValue> result) {
    auto value=Value(success,result);
    if(!value || value->GetType("ok")!=VTYPE_BOOL || !value->GetBool("ok")) {
      const auto code=value && value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"not_visible";
      Fail(code,"The frame is not where the page shows it; scroll or show it first");return;
    }
    auto clicked=CefDictionaryValue::Create();
    clicked->SetBool("ok",true);clicked->SetDouble("x",Number(value,"x"));clicked->SetDouble("y",Number(value,"y"));
    clicked->SetString("message",frame_message_);
    click_(Number(value,"x"),Number(value,"y"));
    Act(clicked);
  }
  // The dictionary a DevTools evaluate answered with.
  static CefRefPtr<CefDictionaryValue> Value(bool success,CefRefPtr<CefDictionaryValue> result) {
    auto remote=success && result?result->GetDictionary("result"):nullptr;
    return remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
  }
  // One field of the command about to be sent, set from an answer.
  void Command(const char* key,double value) { command_->SetDouble(key,value); }
  void Command(const char* key,const std::string& value) { command_->SetString(key,value); }
  // An evaluate of `command_` in the world `context_`.
  CefRefPtr<CefDictionaryValue> Params() const {
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command_);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+
      CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",context_);params->SetBool("returnByValue",true);
    params->SetBool("userGesture",!PrivateText());params->SetBool("silent",true);params->SetBool("awaitPromise",true);
    return params;
  }
  // A page action's own fields, bounded as they arrive, without the document
  // or the world it runs in.
  CefRefPtr<CefDictionaryValue> Fields(const std::string& action) const {
    auto command=CefDictionaryValue::Create();
    command->SetString("action",action);command->SetInt("doc",document_);
    for(const char* key:{"element","text","key","lease","history_scope"}) if(pending_->command->HasKey(key)) command->SetString(key,pending_->command->GetString(key));
    for(const char* key:{"cross_origin","secret","native_caret","enabled"}) if(pending_->command->HasKey(key)) command->SetBool(key,pending_->command->GetBool(key));
    for(const char* key:{"x","y"}) if(pending_->command->HasKey(key)) command->SetDouble(key,Number(pending_->command,key));
    for(const char* key:{"quiet_ms","timeout_ms"}) if(pending_->command->HasKey(key)) command->SetInt(key,pending_->command->GetInt(key));
    if(pending_->command->HasKey("expires_ms")) command->SetDouble("expires_ms",Number(pending_->command,"expires_ms"));
    if(pending_->command->HasKey("values")) command->SetList("values",pending_->command->GetList("values")->Copy());
    return command;
  }
  void Evaluate() {
    auto command=CefDictionaryValue::Create();
    const auto action=Action();
    const bool elementless=!pending_->command->HasKey("element");
    command->SetString("action",action=="click"?"locate":action=="press"?(elementless?"arm":"focus"):action);
    command->SetInt("doc",PrivateText()?text_frame_.document:document_);
    for(const char* key:{"element","text","key","lease","history_scope"}) if(pending_->command->HasKey(key)) command->SetString(key,pending_->command->GetString(key));
    for(const char* key:{"cross_origin","secret","native_caret","enabled"}) if(pending_->command->HasKey(key)) command->SetBool(key,pending_->command->GetBool(key));
    for(const char* key:{"x","y"}) if(pending_->command->HasKey(key)) command->SetDouble(key,Number(pending_->command,key));
    for(const char* key:{"quiet_ms","timeout_ms"}) if(pending_->command->HasKey(key)) command->SetInt(key,pending_->command->GetInt(key));
    if(pending_->command->HasKey("expires_ms")) command->SetDouble("expires_ms",Number(pending_->command,"expires_ms"));
    if(pending_->command->HasKey("values")) command->SetList("values",pending_->command->GetList("values")->Copy());
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",PrivateText()?text_frame_.world:world_);params->SetBool("returnByValue",true);
    // Private focus/check/start/edit does not grant a site transient activation.
    params->SetBool("userGesture",!PrivateText());params->SetBool("silent",true);
    // `settle` answers with a promise; everything else with a value.
    params->SetBool("awaitPromise",true);
    if(pending_->command->GetString("op")=="text_input") params->SetDouble("timeout",5000);
    Call(Stage::Evaluate,"Runtime.evaluate",params);
  }
  void Act(CefRefPtr<CefDictionaryValue> value) {
    const auto action=Action();
    if(PrivateText() && action=="prepare_text_input") {
      auto data=value?value->GetDictionary("data"):nullptr;
      if(!data || data->GetType("lease")!=VTYPE_STRING || data->GetType("multiline")!=VTYPE_BOOL) { Fail("script","The private preparation returned no field policy");return; }
      text_owner_=text_frame_;text_lease_=data->GetString("lease").ToString();
    }
    if(PrivateText() && (action=="cancel_text_input" || action=="commit_text_input" || action=="undo_text_input" || action=="redo_text_input")) { text_owner_.reset();text_lease_.clear(); }
    // An edit runs the page's input handlers.
    if(action=="commit_text_input" || action=="undo_text_input" || action=="redo_text_input") pending_->navigation="possible";
    if(action=="outline" || action=="changes") {
      // The page's outline lists each frame element, whose own lines follow it.
      Succeed(Text(value,"message"),Merged(value));return;
    }
    if(action=="at" || action=="settle" || action=="reader" || action=="prepare_text_input" || action=="start_text_input" || action=="check_text_input" || action=="commit_text_input" || action=="cancel_text_input" || action=="undo_text_input" || action=="redo_text_input") {
      Succeed(Text(value,"message"),std::nullopt,value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr,
        PrivateText() && value->GetType("history")==VTYPE_DICTIONARY?value->GetDictionary("history"):nullptr);return;
    }
    if(action=="click") {
      const auto number=[&](const char* key){return value->GetType(key)==VTYPE_DOUBLE?value->GetDouble(key):value->GetInt(key);};
      // A click inside a frame is at a point of that frame's own view: the
      // document showing it says where that point is on the page.
      if(value->GetType("frame")==VTYPE_STRING) { PlaceClick(number("x"),number("y"),frame_message_);return; }
      click_(number("x"),number("y"));
    } else if(action=="press") {
      auto key=CefDictionaryValue::Create();key->SetString("key",pending_->command->GetString("key"));
      for(const char* flag:{"ctrl","shift","alt","meta"}) key->SetBool(flag,pending_->command->GetBool(flag));
      key_(key);
    }
    pending_->message=action=="press"?"Pressed "+Chord():Text(value,"message");
    // Answers once the page has handled the input: see `fence` in page_automation.js.
    auto params=CefDictionaryValue::Create();
    params->SetString("expression","globalThis.__tiloPage.fence()");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);
    params->SetBool("awaitPromise",true);params->SetBool("silent",true);
    Call(Stage::Fence,"Runtime.evaluate",params);
  }
  // Where a point of a frame's view is on the page, then the click there.
  void PlaceClick(double x,double y,const std::string& message) {
    if(frame_>=frames_.size() || !frames_[frame_].world) {
      Fail("stale_element","That frame is no longer shown on the page; read the page again");return;
    }
    frame_message_=message.empty()?pending_->message:message;
    command_=CefDictionaryValue::Create();
    Command("action",std::string("frame_place"));Command("key",frames_[frame_].key);
    Command("x",x);Command("y",y);
    context_=world_;
    Call(Stage::FramePlace,"Runtime.evaluate",Params());
  }
  void Fenced(const std::string& navigation) {
    pending_->navigation=navigation=="started"?"started":"possible";
    if(pending_->navigation!="started") { Succeed(pending_->message,std::nullopt);return; }
    if(navigations_==pending_->navigations) pending_->awaiting_start=true;
    else if(browser_->IsLoading()) pending_->awaiting_load=true;
    else { Succeed(pending_->message,std::nullopt);return; }
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->Succeed(self->pending_->message,std::nullopt);
    },CefRefPtr<PageAutomation>(this),pending_->token),kLoadWaitMs);
  }
  std::string Chord() const {
    static constexpr std::pair<const char*,const char*> kModifiers[]={{"ctrl","Control+"},{"shift","Shift+"},{"alt","Alt+"},{"meta","Meta+"}};
    std::string chord;const auto& command=pending_->command;
    for(const auto& [flag,name]:kModifiers) if(command->GetBool(flag)) chord+=name;
    const auto key=command->GetString("key").ToString();
    return chord+(key==" "?"Space":key);
  }
  static std::string Text(CefRefPtr<CefDictionaryValue> value,const char* key) {
    return value && value->GetType(key)==VTYPE_STRING?value->GetString(key).ToString():std::string();
  }
  static double Number(CefRefPtr<CefDictionaryValue> d,const char* key) {
    return d->GetType(key)==VTYPE_DOUBLE?d->GetDouble(key):d->GetInt(key);
  }
  void Succeed(const std::string& message,std::optional<std::string> outline,CefRefPtr<CefDictionaryValue> data=nullptr,CefRefPtr<CefDictionaryValue> history=nullptr) {
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",true);
    fields->SetString("url",browser_->GetMainFrame()->GetURL());fields->SetString("message",message.substr(0,1024));
    // A navigation the page asked for that the browser never began is not one.
    const bool navigated=pending_->navigation=="started" && navigations_!=pending_->navigations;
    fields->SetString("navigation",navigated?"started":pending_->navigation=="none"?"none":"possible");
    fields->SetBool("navigated",navigated);fields->SetBool("loading",navigated && browser_->IsLoading());
    if(outline) fields->SetString("outline",*outline);else fields->SetNull("outline");
    if(data) fields->SetDictionary("data",data->Copy(false));
    if(history) fields->SetDictionary("history",history->Copy(false));
    Finish(fields);
  }
  static CefRefPtr<CefDictionaryValue> Failure(const std::string& code,const std::string& message) {
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",false);
    fields->SetString("code",code.empty()?"script":code);fields->SetString("message",message.substr(0,1024));
    return fields;
  }
  void Fail(const std::string& code,const std::string& message) {
    if(PrivateText() && code=="text_input_retired" && text_owner_ && pending_->command->GetString("lease").ToString()==text_lease_) {
      text_owner_.reset();text_lease_.clear();
    }
    Finish(Failure(code,message));
  }
  void Finish(CefRefPtr<CefDictionaryValue> fields) {
    const int request=pending_->request;const bool text_input=pending_->command->GetString("op")=="text_input";
    pending_.reset();call_=0;
    if(native_attempted_) {
      auto params=CefDictionaryValue::Create();params->SetString("objectGroup","tilo-private-native-caret");
      browser_->GetHost()->ExecuteDevToolsMethod(0,"Runtime.releaseObjectGroup",params);
      native_object_.clear();native_attempted_=false;
    }
    Reply(request,fields,text_input);
  }
  void Reply(int request,CefRefPtr<CefDictionaryValue> fields,bool text_input=false) { fields->SetInt("request_id",request);emit_(text_input?"text_input_result":"page_result",fields); }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;
  Emit emit_;Click click_;Key key_;
  std::optional<Pending> pending_;Stage stage_=Stage::FrameTree;int call_=0;
  std::vector<TextFrame> text_candidates_;TextFrame text_frame_;std::optional<TextFrame> text_owner_;std::string text_lease_;
  bool native_attempted_=false;std::string native_object_;
  int world_=0,document_=0;std::string loader_;uint64_t token_=0,navigations_=0;
  // The frames this document shows, the one being read, and the frame an
  // action was routed to. Bounded in frames and depth by kMaxFrames and
  // kMaxFrameDepth; emptied whenever the page's own document changes.
  std::vector<Frame> frames_;size_t frames_at_=0,frame_=0;
  std::string frame_message_;
  // Whether this page action names a ref inside a frame, so it runs there.
  bool frame_route_=false;
  // Whether the document is being read for an outline, which lists the frames.
  bool reading_=false;
  CefRefPtr<CefDictionaryValue> command_;int context_=0;
  IMPLEMENT_REFCOUNTING(PageAutomation);
};
}
