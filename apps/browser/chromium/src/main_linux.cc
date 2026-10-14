#include "host.h"
#include "include/base/cef_compiler_specific.h"
// Every Chromium subprocess forked by the zygote returns through this frame
// when it exits, and Chromium gives each forked child a fresh stack canary
// (`--change-stack-guard-on-fork`). A canary pushed here before the fork no
// longer matches on return, so with the host's -fstack-protector-strong the
// child aborts with "*** stack smashing detected ***" as it exits. Like
// Chromium's own ChromeMain and CEF's CefExecuteProcess, the frame that spans
// the fork carries no canary.
NO_STACK_PROTECTOR int main(int argc,char** argv) {
  int code=CefExecuteProcess(CefMainArgs(argc,argv),MakeRendererApp(),nullptr);
  if(code>=0) return code;
  return RunHost(argc,argv);
}
