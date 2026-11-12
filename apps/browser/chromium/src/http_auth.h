#pragma once
// A server's (or a proxy's) request for a user name and a password: HTTP
// authentication, Basic or Digest.
//
// Chromium asks for the credentials of one request. Who is asked, and
// whether anybody is, is decided here, from what Chromium says of that
// request and what the tab shows:
//
// - The one who asks is named by where the credentials go: the origin of the
//   challenged request, which must be the server that sent the challenge, or
//   the proxy's host and port. The realm is the server's own text; it names
//   nothing, and is cleaned and bounded before anybody is shown it.
// - Only the tab's own document asks, and what a document loads from its own
//   origin. A page of one site cannot have another site's sign-in put in
//   front of the person as if it were its own: a challenge for anything else
//   a page loads (an image, a frame, a request to another origin) is
//   cancelled, as current browsers cancel it.
// - Credentials answer exactly the challenge they were asked for: the answer
//   names the asker it was shown, and one naming another is no answer.
#include <algorithm>
#include <cctype>
#include <string>

namespace tilo {
// The Chromium switch that hands every challenge to the embedder
// (CefRequestHandler::GetAuthCredentials). Without it Chromium asks in a
// prompt of its own: a window on the desktop of whoever runs the host,
// which an offscreen page never shows and nobody can answer. The host sets
// it on every start and does not run without it, and holds or cancels each
// challenge itself: none is ever left to Chromium.
inline constexpr char kHoldSignInsSwitch[]="disable-chrome-login-prompt";
inline constexpr size_t kMaxAuthRealm=256,kMaxAuthScheme=32,kMaxAuthCredential=1024,kMaxAuthAsker=512;
// How long a challenge waits for its answer: a person looks a password up
// for as long as it takes, and a request nobody answers still ends.
inline constexpr int64_t kAuthWaitMs=10*60*1000;

// The challenged request's address, as its parts.
struct AuthRequest {
  std::string scheme,host;
  // The port the address names, or 0 for its scheme's own.
  int port=0;
};

inline std::string AuthLower(std::string text) {
  std::transform(text.begin(),text.end(),text.begin(),[](unsigned char c){return static_cast<char>(std::tolower(c));});
  return text;
}
// A host as it is compared: lower case, an IPv6 literal without its brackets.
inline std::string AuthHost(std::string host) {
  host=AuthLower(std::move(host));
  if(host.size()>=2 && host.front()=='[' && host.back()==']') host=host.substr(1,host.size()-2);
  return host;
}
inline int AuthDefaultPort(const std::string& scheme) { return scheme=="https"?443:scheme=="http"?80:0; }

// Who asks, as the person is shown it and as an answer names it: the
// request's origin (`https://example.org`, `http://127.0.0.1:8080`) for a
// server, `host:port` for a proxy. Empty when nobody may be asked: the
// request is not a web address, or the one who sent the challenge is not
// the server the request went to, so the credentials would go to a host
// other than the one shown.
inline std::string AuthAsker(const AuthRequest& request,bool proxy,const std::string& challenger_host,int challenger_port) {
  if(challenger_host.empty() || challenger_port<=0 || challenger_port>65535) return {};
  if(proxy) return AuthLower(challenger_host)+":"+std::to_string(challenger_port);
  const auto scheme=AuthLower(request.scheme);
  const int usual=AuthDefaultPort(scheme);
  if(!usual || request.host.empty() || request.port<0 || request.port>65535) return {};
  const int port=request.port?request.port:usual;
  if(AuthHost(request.host)!=AuthHost(challenger_host) || port!=challenger_port) return {};
  return scheme+"://"+AuthLower(request.host)+(port==usual?"":":"+std::to_string(port));
}

// An address without its fragment, which names no other request.
inline std::string AuthWithoutFragment(const std::string& url) { return url.substr(0,url.find('#')); }

// Whether the challenge of the request to `url`, asked by `asker`, may be
// put to the person in a tab navigating to `navigating` (empty when it is
// not) whose document has the origin `document` (empty when it has none).
inline bool AuthMayAsk(const std::string& url,const std::string& asker,bool proxy,const std::string& navigating,const std::string& document) {
  if(asker.empty()) return false;
  // A proxy stands between the person and every site: no page chose it.
  if(proxy) return true;
  // The tab's own document, on its way.
  if(!navigating.empty() && AuthWithoutFragment(url)==AuthWithoutFragment(navigating)) return true;
  // What the document loads from its own origin.
  return !document.empty() && asker==document;
}

// The realm as it is shown: the server's text without control characters,
// trimmed, at most kMaxAuthRealm bytes and cut where a character starts.
inline std::string AuthRealm(std::string realm) {
  for(auto& c:realm) if(static_cast<unsigned char>(c)<0x20 || c==0x7f) c=' ';
  const auto first=realm.find_first_not_of(' ');
  if(first==std::string::npos) return {};
  realm=realm.substr(first,realm.find_last_not_of(' ')-first+1);
  if(realm.size()>kMaxAuthRealm) {
    size_t end=kMaxAuthRealm;
    while(end>0 && (static_cast<unsigned char>(realm[end])&0xC0)==0x80) --end;
    realm.resize(end);
  }
  return realm;
}

// The scheme's name (`basic`, `digest`) in lower case, or empty for one that
// is no plain token.
inline std::string AuthScheme(std::string scheme) {
  scheme=AuthLower(std::move(scheme));
  const bool token=!scheme.empty() && scheme.size()<=kMaxAuthScheme &&
    std::all_of(scheme.begin(),scheme.end(),[](unsigned char c){return std::isalnum(c) || c=='-';});
  return token?scheme:std::string();
}

// What tells one question from another: the same asker, realm and scheme is
// the same question, however many requests wait on it.
inline std::string AuthQuestion(const std::string& asker,bool proxy,const std::string& realm,const std::string& scheme) {
  return std::string(proxy?"proxy ":"server ")+asker+"\n"+scheme+"\n"+realm;
}
}
