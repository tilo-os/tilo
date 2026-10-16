#include "devtools_bridge.h"
#include <cassert>
int main() {
  assert(tilo::DevToolsMethodAllowed("Extensions.loadUnpacked"));
  assert(tilo::DevToolsMethodAllowed("Extensions.uninstall"));
  assert(!tilo::DevToolsMethodAllowed("Browser.close"));
  assert(!tilo::DevToolsMethodAllowed("Page.navigate"));
  assert(!tilo::DevToolsMethodAllowed("Extensions"));
  tilo::DevToolsPending pending;
  auto first=pending.Add(3,7);auto second=pending.Add(4,7);
  assert(first && second && *first!=*second);
  auto owner=pending.Take(*first);
  assert(owner && owner->tab==3 && owner->request_id==7);
  assert(!pending.Take(*first));
  for(size_t i=0;i<tilo::kMaxPendingDevTools-1;++i) assert(pending.Add(1,1));
  assert(!pending.Add(1,1));
  pending.Forget(*second);
  assert(pending.Add(1,1));
  std::string buffer=std::string("{\"id\":1}")+'\0'+"{\"id\":2}"+'\0'+"{\"id\"";
  std::string out[8];size_t count=0;
  assert(tilo::TakeDevToolsMessages(buffer,out,8,&count));
  assert(count==2 && out[0]=="{\"id\":1}" && out[1]=="{\"id\":2}" && buffer=="{\"id\"");
  std::string huge(tilo::kMaxDevToolsMessage+2,'x');
  assert(!tilo::TakeDevToolsMessages(huge,out,8,&count));
  return 0;
}
