#include "site_policy.h"
#include "host.h"
#include "credential_forms.h"
#include "credential_document.h"
#include "renderer_media_lifetime.h"
#include "cosmetics.h"
#include "page_clipboard.h"
#include "include/cef_browser.h"
#include "include/cef_command_line.h"
#include "include/cef_dom.h"
#include "include/cef_frame.h"
#include "include/cef_parser.h"
#include "include/cef_process_message.h"
#include "include/cef_render_process_handler.h"
#include "include/cef_v8.h"
#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <unistd.h>
#include <functional>
#include <map>
#include <vector>

namespace {
constexpr size_t kMaxMediaFrames=64;
constexpr size_t kMaxSelectionText=1024*1024;
// Reads the document's selection as text, the way a copy would.
class SelectionReader final : public CefDOMVisitor {
 public:
  explicit SelectionReader(std::string* text):text_(text) {}
  void Visit(CefRefPtr<CefDOMDocument> document) override { if(document && document->HasSelection()) *text_=document->GetSelectionAsText().ToString(); }
 private:
  std::string* text_;
  IMPLEMENT_REFCOUNTING(SelectionReader);
};
// What the focused node takes, for the input method: 0 nothing, then text,
// multiline text, a password, an email address, an address, a telephone
// number, a number and a search (the browser's purposes, host.cc).
int TextFieldKind(CefRefPtr<CefDOMNode> node) {
  if(!node || !node->IsEditable() || node->HasElementAttribute("readonly") || node->HasElementAttribute("disabled")) return 0;
  // An editable node that is no form control is an editable document.
  if(!node->IsFormControlElement()) return 2;
  switch(node->GetFormControlElementType()) {
    case DOM_FORM_CONTROL_TYPE_TEXT_AREA: return 2;
    case DOM_FORM_CONTROL_TYPE_INPUT_PASSWORD: return 3;
    case DOM_FORM_CONTROL_TYPE_INPUT_EMAIL: return 4;
    case DOM_FORM_CONTROL_TYPE_INPUT_URL: return 5;
    case DOM_FORM_CONTROL_TYPE_INPUT_TELEPHONE: return 6;
    case DOM_FORM_CONTROL_TYPE_INPUT_NUMBER: return 7;
    case DOM_FORM_CONTROL_TYPE_INPUT_SEARCH: return 8;
    default: return 1;
  }
}
// Tells the browser what the focused node in `frame` takes and where it is,
// in the widget's device pixels. A box is given only for a frame whose
// ancestors are all in this process, so the widget places it: a field in a
// frame of another site goes without one. `probe` marks an answer to the
// browser's question rather than a focus change.
void ReportTextField(CefRefPtr<CefFrame> frame,CefRefPtr<CefDOMNode> node,bool probe) {
  if(!frame || !frame->IsValid()) return;
  auto message=CefProcessMessage::Create("tilo.text_field");auto args=message->GetArgumentList();
  const int kind=TextFieldKind(node);
  args->SetBool(0,probe);args->SetInt(1,kind);
  bool placed=false;
  for(auto ancestor=frame;ancestor;ancestor=ancestor->GetParent()) if(ancestor->IsMain()) {placed=true;break;}
  if(kind && placed) {
    const CefRect box=node->GetElementBounds();
    const auto bounded=[](int value){return std::clamp(value,-65536,65536);};
    args->SetInt(2,bounded(box.x));args->SetInt(3,bounded(box.y));args->SetInt(4,bounded(box.width));args->SetInt(5,bounded(box.height));
  }
  frame->SendProcessMessage(PID_BROWSER,message);
}
class FocusedFieldReader final : public CefDOMVisitor {
 public:
  explicit FocusedFieldReader(CefRefPtr<CefFrame> frame):frame_(frame) {}
  void Visit(CefRefPtr<CefDOMDocument> document) override { ReportTextField(frame_,document?document->GetFocusedNode():nullptr,true); }
 private:
  CefRefPtr<CefFrame> frame_;
  IMPLEMENT_REFCOUNTING(FocusedFieldReader);
};
void TraceCredential(const char* stage,int browser=0) {
  static const bool enabled=[] {const char* value=std::getenv("TILO_WEBVIEW_PROFILE");return value && std::strcmp(value,"trace")==0;}();
  static unsigned count=0;
  if(enabled && count++<64) std::fprintf(stderr,"[chromium-credential-renderer] pid=%d browser=%d %s\n",int(getpid()),browser,stage);
}
// Deliberately require a direct web document. Inherited about:blank/srcdoc and
// blob documents are excluded: a URL alone cannot prove their creator origin.
// This also excludes chrome:, chrome-extension:, devtools:, file: and data:.
bool IsWebDocument(CefRefPtr<CefFrame> frame) {
  CefURLParts parts;
  if (!frame || !CefParseURL(frame->GetURL(), parts)) return false;
  const auto scheme=CefString(&parts.scheme).ToString();
  return scheme=="http" || scheme=="https";
}
// An extension's own page (its popup, its settings): it copies and pastes
// as a web page does.
bool IsExtensionDocument(CefRefPtr<CefFrame> frame) {
  CefURLParts parts;
  return frame && CefParseURL(frame->GetURL(), parts) && CefString(&parts.scheme).ToString()=="chrome-extension";
}
// A document a page wrote into one of its frames (about:blank,
// about:srcdoc), as rich editors write the one they edit in: what is copied
// and pasted there is the page's. The host's gates (the person's activation
// for a copy, the site's setting for a read) do not rest on the document's
// address. A tab's own blank page is no page's.
bool IsWrittenDocument(CefRefPtr<CefFrame> frame) {
  CefURLParts parts;
  if(!frame || frame->IsMain() || !CefParseURL(frame->GetURL(), parts) || CefString(&parts.scheme).ToString()!="about") return false;
  const auto path=CefString(&parts.path).ToString();
  return path=="blank" || path=="srcdoc";
}
// Whether this renderer's engine has no clipboard of its own (page_clipboard.h).
bool PageClipboard() {
  static const bool enabled=CefCommandLine::GetGlobalCommandLine()->HasSwitch(tilo::kPageClipboardSwitch);
  return enabled;
}
// What page_clipboard.js calls: `send` with a copy, `ask` with a callback
// for the system clipboard's text.
class ClipboardHandler final : public CefV8Handler {
 public:
  using Ask=std::function<void(CefRefPtr<CefFrame>,CefRefPtr<CefV8Context>,CefRefPtr<CefV8Value>)>;
  ClipboardHandler(CefRefPtr<CefFrame> frame,Ask ask):frame_(frame),ask_(std::move(ask)) {}
  bool Execute(const CefString& name,CefRefPtr<CefV8Value>,const CefV8ValueList& args,CefRefPtr<CefV8Value>&,CefString&) override {
    if(!frame_ || !frame_->IsValid() || args.size()!=1) return true;
    if(name=="ask") { if(args[0]->IsFunction()) ask_(frame_,CefV8Context::GetCurrentContext(),args[0]);return true; }
    auto copy=args[0];
    if(!copy->IsObject()) return true;
    auto text=copy->GetValue("text"),html=copy->GetValue("html"),secret=copy->GetValue("secret");
    if(!text || !text->IsString() || !html || !html->IsString() || !secret || !secret->IsBool()) return true;
    const auto words=text->GetStringValue().ToString();
    if(words.empty() || words.size()>tilo::kMaxClipboardText) return true;
    auto markup=html->GetStringValue().ToString();
    if(markup.size()>tilo::kMaxClipboardHtml) markup.clear();
    auto message=CefProcessMessage::Create("tilo.clipboard_write");auto fields=message->GetArgumentList();
    fields->SetString(0,words);fields->SetString(1,markup);fields->SetBool(2,secret->GetBoolValue());
    frame_->SendProcessMessage(PID_BROWSER,message);return true;
  }
 private:
  CefRefPtr<CefFrame> frame_;Ask ask_;
  IMPLEMENT_REFCOUNTING(ClipboardHandler);
};
class MediaStateHandler final : public CefV8Handler {
 public: MediaStateHandler(CefRefPtr<CefFrame> frame,uint64_t token):frame_(frame),token_(token) {}
  bool Execute(const CefString&,CefRefPtr<CefV8Value>,const CefV8ValueList& args,CefRefPtr<CefV8Value>&,CefString&) override {
    if(!frame_ || !frame_->IsValid() || args.size()!=1 || !args[0]->IsBool()) return false;
    auto message=CefProcessMessage::Create("tilo.media_state");message->GetArgumentList()->SetBool(0,args[0]->GetBoolValue());message->GetArgumentList()->SetDouble(1,static_cast<double>(token_));frame_->SendProcessMessage(PID_BROWSER,message);return true;
  }
 private: CefRefPtr<CefFrame> frame_;uint64_t token_; IMPLEMENT_REFCOUNTING(MediaStateHandler);
};
struct MediaContext { CefRefPtr<CefBrowser> browser;CefRefPtr<CefV8Context> context;CefRefPtr<CefFrame> frame;uint64_t token;bool main; };
std::map<int,std::map<std::string,MediaContext>> media_contexts;
}

class Renderer final : public CefApp, public CefRenderProcessHandler {
 public:
  CefRefPtr<CefRenderProcessHandler> GetRenderProcessHandler() override { return this; }

  void OnContextCreated(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,CefRefPtr<CefV8Context> context) override {
    TraceCredential(IsWebDocument(frame)?"web-context-created":"nonweb-context-created",browser->GetIdentifier());
    // The page's own world only: the host's isolated worlds share its events.
    if(PageClipboard() && (IsWebDocument(frame) || IsExtensionDocument(frame) || IsWrittenDocument(frame))) {
      auto main=frame->GetV8Context();
      if(main && main->IsSame(context)) InstallClipboard(frame,context);
    }
    if(!IsWebDocument(frame)) return;
    if(frame->IsMain() && credential_context_sequence_<9007199254740991ULL) {
      CefRefPtr<CefV8Value> forms;
      CefRefPtr<CefV8Exception> exception;
      if(context->Enter()) {
        context->Eval(tilo::kCredentialForms,frame->GetURL(),0,forms,exception);
        context->Exit();
      }
      TraceCredential(forms && forms->IsObject()?"forms-created":"forms-unavailable");
      if(forms && forms->IsObject()) {
        credential_contexts_[browser->GetIdentifier()]={browser,context,frame,
            std::to_string(++credential_context_sequence_),{},forms};
        TraceCredential("forms-stored",browser->GetIdentifier());
      }
    }
    auto& config=configs_[browser->GetIdentifier()];
    // A renderer process may contain only a cross-origin child; its remote
    // main frame cannot identify the top-level origin. Ask the browser for a
    // document-correlated policy instead of applying the previous page's cache.
    const auto filter_token=++filter_sequence_;filter_tokens_[frame->GetIdentifier().ToString()]=filter_token;
    auto config_request=CefProcessMessage::Create("tilo.config");config_request->GetArgumentList()->SetDouble(0,static_cast<double>(filter_token));frame->SendProcessMessage(PID_BROWSER,config_request);
    {
      auto& browser_media=media_contexts[browser->GetIdentifier()];
      const auto key=frame->GetIdentifier().ToString();
      if(browser_media.find(key)==browser_media.end()) {
        const bool has_main=std::any_of(browser_media.begin(),browser_media.end(),[](const auto& entry){return entry.second.main;});
        if(!frame->IsMain() && browser_media.size()>=kMaxMediaFrames-(has_main?0:1)) return;
        if(frame->IsMain() && browser_media.size()>=kMaxMediaFrames) {auto victim=std::find_if(browser_media.begin(),browser_media.end(),[](const auto& entry){return !entry.second.main;});if(victim==browser_media.end())return;auto closed=CefProcessMessage::Create("tilo.media_context_closed");closed->GetArgumentList()->SetDouble(0,static_cast<double>(victim->second.token));victim->second.frame->SendProcessMessage(PID_BROWSER,closed);browser_media.erase(victim);}
      }
      const uint64_t token=++media_context_counter_;
      browser_media[key]={browser,context,frame,token,frame->IsMain()};
      auto media_context=CefProcessMessage::Create("tilo.media_context");media_context->GetArgumentList()->SetDouble(0,static_cast<double>(token));frame->SendProcessMessage(PID_BROWSER,media_context);
      context->Enter();context->GetGlobal()->SetValue("__tiloMediaState",CefV8Value::CreateFunction("__tiloMediaState",new MediaStateHandler(frame,token)),V8_PROPERTY_ATTRIBUTE_DONTDELETE);
      CefRefPtr<CefV8Value> result;CefRefPtr<CefV8Exception> exception;
      context->Eval("(()=>{let playing=null;const update=()=>{const next=[...document.querySelectorAll('audio,video')].some(e=>!e.paused&&!e.ended&&e.readyState>1);if(next!==playing){playing=next;__tiloMediaState(next)}};for(const event of ['play','playing','canplay','pause','ended','emptied'])addEventListener(event,update,true);update()})()",frame->GetURL(),0,result,exception);context->Exit();
    }
  }
  void OnFocusedNodeChanged(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,CefRefPtr<CefDOMNode> node) override {
    ReportTextField(frame,node,false);
  }
  void OnBrowserDestroyed(CefRefPtr<CefBrowser> browser) override {
    TraceCredential("browser-destroyed",browser->GetIdentifier());
    // CEF can create a replacement handle with the same numeric ID before
    // destroying the old handle during cross-origin navigation.
    auto credential=credential_contexts_.find(browser->GetIdentifier());
    if(credential!=credential_contexts_.end() && credential->second.browser->IsSame(browser))
      credential_contexts_.erase(credential);
    tilo::EraseMediaBrowserContexts(media_contexts,browser);configs_.erase(browser->GetIdentifier());
  }
  void OnContextReleased(CefRefPtr<CefBrowser> browser, CefRefPtr<CefFrame> frame,
                         CefRefPtr<CefV8Context> context) override {
    auto credential=credential_contexts_.find(browser->GetIdentifier());
    if(credential!=credential_contexts_.end() && credential->second.context->IsSame(context)) {
      if(!credential->second.generation.empty()) {
        auto message=CefProcessMessage::Create("tilo.credential_context_closed");
        auto args=message->GetArgumentList();args->SetString(0,credential->second.generation);
        args->SetString(1,credential->second.token);args->SetString(2,frame->GetURL());
        frame->SendProcessMessage(PID_BROWSER,message);
      }
      TraceCredential("forms-context-released");
      credential_contexts_.erase(credential);
    }
    auto key=frame->GetIdentifier().ToString();
    auto browser_media=media_contexts.find(browser->GetIdentifier());
    if(browser_media!=media_contexts.end()) {auto media=browser_media->second.find(key);if(media!=browser_media->second.end() && media->second.context->IsSame(context)) {auto message=CefProcessMessage::Create("tilo.media_context_closed");message->GetArgumentList()->SetDouble(0,static_cast<double>(media->second.token));frame->SendProcessMessage(PID_BROWSER,message);browser_media->second.erase(media);}if(browser_media->second.empty())media_contexts.erase(browser_media);}
    filter_tokens_.erase(key);
    auto clipboard=clipboards_.find(key);
    if(clipboard!=clipboards_.end() && clipboard->second.context->IsSame(context)) clipboards_.erase(clipboard);
    for(auto it=clipboard_asks_.begin();it!=clipboard_asks_.end();) it=it->second.context->IsSame(context)?clipboard_asks_.erase(it):std::next(it);
    auto cosmetic=cosmetics_.find(key);
    if(cosmetic!=cosmetics_.end() && cosmetic->second.context->IsSame(context)) cosmetics_.erase(cosmetic);
    auto found=applied_.find(key);
    if(found!=applied_.end() && found->second.context->IsSame(context)) applied_.erase(found);
  }

  bool OnProcessMessageReceived(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,
                                CefProcessId source,CefRefPtr<CefProcessMessage> message) override {
    if(source!=PID_BROWSER) return false;
    if(message->GetName()=="tilo.credential_action") {
      CredentialAction(browser,frame,message->GetArgumentList());return true;
    }
    if(message->GetName()=="tilo.text_field_probe") {
      if(frame && frame->IsValid()) frame->VisitDOM(new FocusedFieldReader(frame));
      return true;
    }
    if(message->GetName()=="tilo.credential_probe") {
      TraceCredential("probe-received",browser->GetIdentifier());
      auto args=message->GetArgumentList();
      auto found=credential_contexts_.find(browser->GetIdentifier());
      if(!frame || !frame->IsMain() || !frame->IsValid() || args->GetSize()!=2 ||
         args->GetType(0)!=VTYPE_STRING || args->GetType(1)!=VTYPE_STRING ||
         !tilo::ValidCredentialCounter(args->GetString(0).ToString()) ||
         args->GetString(1)!=frame->GetURL() || found==credential_contexts_.end()) {
        TraceCredential(!frame?"probe-no-frame":!frame->IsMain()?"probe-not-main":!frame->IsValid()?"probe-invalid-frame":args->GetSize()!=2?"probe-argument-count":args->GetType(0)!=VTYPE_STRING?"probe-generation-type":args->GetType(1)!=VTYPE_STRING?"probe-url-type":!tilo::ValidCredentialCounter(args->GetString(0).ToString())?"probe-invalid-generation":args->GetString(1)!=frame->GetURL()?"probe-url-mismatch":"probe-missing-forms");return true;}
      auto live=frame->GetV8Context();
      if(!found->second.browser->IsSame(browser) || !live || !live->IsValid() || !found->second.context->IsSame(live) ||
         found->second.frame->GetIdentifier()!=frame->GetIdentifier()) {
        TraceCredential(!live?"probe-no-context":!live->IsValid()?"probe-invalid-context":!found->second.context->IsSame(live)?"probe-context-mismatch":"probe-frame-mismatch");return true;}
      TraceCredential("probe-accepted");
      found->second.generation=args->GetString(0).ToString();
      auto response=CefProcessMessage::Create("tilo.credential_context");
      auto fields=response->GetArgumentList();fields->SetString(0,found->second.generation);
      fields->SetString(1,found->second.token);fields->SetString(2,frame->GetURL());
      frame->SendProcessMessage(PID_BROWSER,response);return true;
    }
    if(message->GetName()=="tilo.selection") {
      auto args=message->GetArgumentList();
      if(!frame || !frame->IsValid() || args->GetSize()!=1 || args->GetType(0)!=VTYPE_INT) return true;
      const int request=args->GetInt(0);
      std::string text;
      frame->VisitDOM(new SelectionReader(&text));
      // A selection is bounded like every message to the embedder.
      if(text.length()>kMaxSelectionText) text.resize(kMaxSelectionText);
      auto reply=CefProcessMessage::Create("tilo.selection_text");
      reply->GetArgumentList()->SetInt(0,request);reply->GetArgumentList()->SetString(1,text);
      frame->SendProcessMessage(PID_BROWSER,reply);return true;
    }
    if(message->GetName()=="tilo.clipboard_text") {ClipboardText(message->GetArgumentList());return true;}
    if(message->GetName()=="tilo.clipboard_prime") {ClipboardPrime(frame,message->GetArgumentList());return true;}
    auto& config=configs_[browser->GetIdentifier()];
    if(message->GetName()=="tilo.filters") {
      auto args=message->GetArgumentList();auto found=filter_tokens_.find(frame->GetIdentifier().ToString());
      if(found==filter_tokens_.end() || args->GetString(4)!=frame->GetURL() || (args->GetDouble(5)>0 && static_cast<uint64_t>(args->GetDouble(5))!=found->second))return true;
      config.filters=args->Copy();ApplyFilters(frame,config.filters);return true;
    }
    if(message->GetName()!="tilo.scripts") return false;
    config.scripts.clear();auto args=message->GetArgumentList();
    for(size_t i=0;i<args->GetSize();++i) config.scripts.push_back(args->GetString(i));
    ApplyScripts(frame,config.scripts);return true;
  }
 private:
  // page_clipboard.js in a document: what keeps the system clipboard's text
  // for its next paste.
  struct Clipboard { CefRefPtr<CefV8Context> context;CefRefPtr<CefV8Value> prime; };
  std::map<std::string,Clipboard> clipboards_;
  // A document's reads of the system clipboard the host has yet to answer.
  struct ClipboardAsk { CefRefPtr<CefV8Context> context;CefRefPtr<CefV8Value> callback; };
  std::map<int,ClipboardAsk> clipboard_asks_;
  int clipboard_ask_=0;
  void InstallClipboard(CefRefPtr<CefFrame> frame,CefRefPtr<CefV8Context> context) {
    if(!context->Enter()) return;
    CefRefPtr<CefV8Value> factory;CefRefPtr<CefV8Exception> exception;
    if(context->Eval(tilo::kPageClipboard,frame->GetURL(),0,factory,exception) && factory && factory->IsFunction()) {
      auto ask=[this](CefRefPtr<CefFrame> frame,CefRefPtr<CefV8Context> context,CefRefPtr<CefV8Value> callback) {Ask(frame,context,callback);};
      CefRefPtr<ClipboardHandler> handler=new ClipboardHandler(frame,ask);
      CefV8ValueList arguments{CefV8Value::CreateFunction("send",handler),CefV8Value::CreateFunction("ask",handler)};
      auto prime=factory->ExecuteFunction(nullptr,arguments);
      if(prime && prime->IsFunction()) clipboards_[frame->GetIdentifier().ToString()]={context,prime};
    }
    context->Exit();
  }
  void Ask(CefRefPtr<CefFrame> frame,CefRefPtr<CefV8Context> context,CefRefPtr<CefV8Value> callback) {
    // A document reads one at a time in practice; a flood is answered with
    // nothing rather than kept.
    if(!context || clipboard_asks_.size()>=16) { callback->ExecuteFunction(nullptr,{CefV8Value::CreateNull()});return; }
    clipboard_ask_=clipboard_ask_%2147483647+1;
    clipboard_asks_[clipboard_ask_]={context,callback};
    auto message=CefProcessMessage::Create("tilo.clipboard_read");message->GetArgumentList()->SetInt(0,clipboard_ask_);
    frame->SendProcessMessage(PID_BROWSER,message);
  }
  void ClipboardText(CefRefPtr<CefListValue> args) {
    if(args->GetSize()!=2 || args->GetType(0)!=VTYPE_INT) return;
    auto found=clipboard_asks_.find(args->GetInt(0));
    if(found==clipboard_asks_.end()) return;
    auto ask=found->second;clipboard_asks_.erase(found);
    if(!ask.context->IsValid() || !ask.context->Enter()) return;
    auto text=args->GetType(1)==VTYPE_STRING?CefV8Value::CreateString(args->GetString(1)):CefV8Value::CreateNull();
    ask.callback->ExecuteFunction(nullptr,{text});
    ask.context->Exit();
  }
  void ClipboardPrime(CefRefPtr<CefFrame> frame,CefRefPtr<CefListValue> args) {
    if(!frame || !frame->IsValid() || args->GetSize()!=2 || args->GetType(0)!=VTYPE_INT || args->GetType(1)!=VTYPE_STRING) return;
    auto found=clipboards_.find(frame->GetIdentifier().ToString());
    if(found!=clipboards_.end() && found->second.context->IsValid() && found->second.context->Enter()) {
      const auto clipboard=found->second;
      clipboard.prime->ExecuteFunction(nullptr,{CefV8Value::CreateString(args->GetString(1))});
      clipboard.context->Exit();
    }
    // Kept or not, the key goes on to the page now.
    auto reply=CefProcessMessage::Create("tilo.clipboard_primed");reply->GetArgumentList()->SetInt(0,args->GetInt(0));
    frame->SendProcessMessage(PID_BROWSER,reply);
  }
  struct CredentialContext {
    CefRefPtr<CefBrowser> browser;
    CefRefPtr<CefV8Context> context;
    CefRefPtr<CefFrame> frame;
    std::string token, generation;
    CefRefPtr<CefV8Value> forms;
  };
  std::map<int,CredentialContext> credential_contexts_;
  uint64_t credential_context_sequence_=0;
  void CredentialAction(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,CefRefPtr<CefListValue> args) {
    if(!frame || !frame->IsMain() || !frame->IsValid() || args->GetSize()!=4 ||
       args->GetType(0)!=VTYPE_STRING || args->GetType(1)!=VTYPE_STRING ||
       args->GetType(2)!=VTYPE_STRING || args->GetType(3)!=VTYPE_DICTIONARY ||
       !tilo::ValidCredentialCounter(args->GetString(0).ToString()) ||
       !tilo::ValidCredentialCounter(args->GetString(2).ToString()))return;
    auto found=credential_contexts_.find(browser->GetIdentifier());
    if(found==credential_contexts_.end())return;
    // Copy ref-counted context state before calling JS, which can navigate and
    // synchronously invalidate the map entry through input handlers.
    const auto bound=found->second;
    auto live=frame->GetV8Context();
    if(!bound.browser->IsSame(browser) || bound.generation!=args->GetString(0).ToString() || bound.token!=args->GetString(1).ToString() ||
       !live || !live->IsValid() || !bound.context->IsSame(live) ||
       bound.frame->GetIdentifier()!=frame->GetIdentifier() ||
       !tilo::Origin(frame->GetURL()).starts_with("https://"))return;
    auto command=args->GetDictionary(3);
    if(command->GetType("action")!=VTYPE_STRING)return;
    const auto action=command->GetString("action").ToString();
    const bool discover=action=="discover" || action=="discover_addresses";
    const bool validate=action=="validate",capture=action=="capture",fill=action=="fill",address=action=="fill_address";
    if(!discover && !validate && !capture && !fill && !address)return;
    CefDictionaryValue::KeyList keys;command->GetKeys(keys);
    const size_t expected=discover?1:(capture || validate)?2:fill?4:3;
    if(keys.size()!=expected)return;
    for(const auto& key:keys) {
      if(key=="action")continue;
      if(key=="form" && !discover)continue;
      if(fill && (key=="username" || key=="password"))continue;
      if(address && key=="profile")continue;
      return;
    }
    CefV8ValueList arguments;
    if(!discover) {
      if(command->GetType("form")!=VTYPE_STRING || !tilo::ValidCredentialCounter(command->GetString("form").ToString()))return;
    }
    if(fill) {
      if(command->GetType("username")!=VTYPE_STRING || command->GetString("username").length()>1024 ||
         command->GetType("password")!=VTYPE_STRING || command->GetString("password").empty() ||
         command->GetString("password").length()>4096)return;
    }
    if(!bound.context->Enter())return;
    if(!discover)arguments.push_back(CefV8Value::CreateString(command->GetString("form")));
    if(fill) {
      arguments.push_back(CefV8Value::CreateString(command->GetString("username")));
      arguments.push_back(CefV8Value::CreateString(command->GetString("password")));
    }
    if(address) {
      if(command->GetType("profile")!=VTYPE_DICTIONARY){bound.context->Exit();return;}
      auto profile=command->GetDictionary("profile");CefDictionaryValue::KeyList fields;profile->GetKeys(fields);
      if(fields.empty() || fields.size()>9){bound.context->Exit();return;}
      auto object=CefV8Value::CreateObject(nullptr,nullptr);
      for(const auto& field:fields) {
        const auto key=field.ToString();
        if((key!="full_name" && key!="given_name" && key!="family_name" && key!="street" &&
            key!="city" && key!="region" && key!="postal_code" && key!="country") ||
           profile->GetType(field)!=VTYPE_STRING || profile->GetString(field).length()>1024) {
          bound.context->Exit();return;
        }
        object->SetValue(field,CefV8Value::CreateString(profile->GetString(field)),V8_PROPERTY_ATTRIBUTE_NONE);
      }
      arguments.push_back(object);
    }
    const auto method=action=="discover_addresses"?"discoverAddresses":address?"fillAddress":action;
    auto function=bound.forms->GetValue(method);
    auto value=function && function->IsFunction()?function->ExecuteFunctionWithContext(bound.context,nullptr,arguments):nullptr;
    auto result=CefDictionaryValue::Create();result->SetBool("success",false);
    if(discover && value && value->IsArray() && value->GetArrayLength()<=32) {
      auto forms=CefListValue::Create();bool valid=true;
      for(int i=0;i<value->GetArrayLength();++i) {
        auto item=value->GetValue(i);if(!item || !item->IsObject()){valid=false;break;}
        auto id=item->GetValue("id");
        if(!id || !id->IsString() || !tilo::ValidCredentialCounter(id->GetStringValue().ToString())){valid=false;break;}
        auto entry=CefDictionaryValue::Create();entry->SetString("id",id->GetStringValue());
        if(action=="discover") {
          auto username=item->GetValue("username"),kind=item->GetValue("kind");
          if(!username || !username->IsString() || username->GetStringValue().length()>1024 ||
             !kind || !kind->IsString() || (kind->GetStringValue()!="current-password" && kind->GetStringValue()!="new-password")){valid=false;break;}
          entry->SetString("username",username->GetStringValue());entry->SetString("kind",kind->GetStringValue());
        }
        forms->SetDictionary(forms->GetSize(),entry);
      }
      if(valid){result->SetBool("success",true);result->SetList("forms",forms);}
    } else if(capture && value && value->IsObject()) {
      auto username=value->GetValue("username"),password=value->GetValue("password");
      if(username && username->IsString() && username->GetStringValue().length()<=1024 &&
         password && password->IsString() && !password->GetStringValue().empty() && password->GetStringValue().length()<=4096) {
        result->SetBool("success",true);result->SetString("username",username->GetStringValue());
        result->SetString("password",password->GetStringValue());
      }
      value->SetValue("password",CefV8Value::CreateUndefined(),V8_PROPERTY_ATTRIBUTE_NONE);
    } else if((fill || address || validate) && value && value->IsBool())result->SetBool("success",value->GetBoolValue());
    bound.context->Exit();
    auto current=credential_contexts_.find(browser->GetIdentifier());
    if(current==credential_contexts_.end() || current->second.generation!=bound.generation ||
       current->second.token!=bound.token || !bound.context->IsValid())return;
    auto reply=CefProcessMessage::Create("tilo.credential_result");auto fields=reply->GetArgumentList();
    fields->SetString(0,bound.generation);fields->SetString(1,bound.token);
    fields->SetString(2,args->GetString(2));fields->SetDictionary(3,result);
    frame->SendProcessMessage(PID_BROWSER,reply);
  }
  void ApplyScripts(CefRefPtr<CefFrame> frame,const std::vector<CefString>& scripts) {
    if(!IsWebDocument(frame)) return;
    auto context=frame->GetV8Context();if(!context || !context->IsValid()) return;
    auto& applied=applied_[frame->GetIdentifier().ToString()];
    if(applied.context && applied.context->IsSame(context) && applied.scripts==scripts) return;
    applied={context,scripts};
    // Live updates evaluate trusted host source directly. The source does not
    // call page eval(), so CSP remains enforced for the page's own scripts.
    context->Enter();
    for(const auto& script:scripts) {
      if(!IsWebDocument(frame)) break;
      CefRefPtr<CefV8Value> result;CefRefPtr<CefV8Exception> exception;
      context->Eval(script,frame->GetURL(),0,result,exception);
    }
    context->Exit();
  }
  struct Config { CefRefPtr<CefListValue> filters;std::vector<CefString> scripts; };
  std::map<int,Config> configs_;
  uint64_t media_context_counter_=0,filter_sequence_=0;
  std::map<std::string,uint64_t> filter_tokens_;
  void ApplyFilters(CefRefPtr<CefFrame> frame,CefRefPtr<CefListValue> args) {
    if(!IsWebDocument(frame)) return;
    auto context=frame->GetV8Context();if(!context || !context->IsValid()) return;
    const auto rules=args->GetString(0).ToString();bool enabled=args->GetBool(1);
    const auto origin=args->GetString(3).ToString();
    auto exceptions=args->GetList(2);if(exceptions) for(size_t i=0;i<exceptions->GetSize();++i) if(exceptions->GetString(i)==origin) enabled=false;
    auto& applied=cosmetics_[frame->GetIdentifier().ToString()];
    if(applied.context && applied.context->IsSame(context) && applied.rules==rules && applied.enabled==enabled) return;
    context->Enter();
    if(applied.context && applied.context->IsSame(context) && applied.cleanup) applied.cleanup->ExecuteFunction(nullptr,{});
    applied={context,nullptr,rules,enabled};
    if(enabled) {
      if(!blocker_ || rules!=rules_) { blocker_=std::make_shared<tilo::Blocker>(rules);rules_=rules; }
      if(blocker_->Valid()) {
        CefRefPtr<CefV8Value> factory;CefRefPtr<CefV8Exception> exception;
        auto resources=blocker_->Cosmetics(frame->GetURL(),"[]","[]");
        if(resources) {
          const auto script=resources->GetString("injected_script").ToString();
          if(!script.empty()) context->Eval("(function(){const scriptletGlobals={};\n"+script+"\n})()",frame->GetURL(),0,factory,exception);
        }
        if(context->Eval(tilo::kCosmetics,frame->GetURL(),0,factory,exception) && factory && factory->IsFunction()) {
          auto query=CefV8Value::CreateFunction("query",new tilo::CosmeticQuery(blocker_,frame->GetURL()));
          applied.cleanup=factory->ExecuteFunction(nullptr,{query});
        }
      }
    }
    context->Exit();
  }
  struct CosmeticApplied { CefRefPtr<CefV8Context> context;CefRefPtr<CefV8Value> cleanup;std::string rules;bool enabled; };
  std::map<std::string,CosmeticApplied> cosmetics_;
  std::shared_ptr<tilo::Blocker> blocker_;std::string rules_;
  struct Applied { CefRefPtr<CefV8Context> context; std::vector<CefString> scripts; };
  std::map<std::string,Applied> applied_;
  IMPLEMENT_REFCOUNTING(Renderer);
};
CefRefPtr<CefApp> MakeRendererApp() { return new Renderer; }
