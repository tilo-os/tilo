#include "include/cef_sandbox_mac.h"
#include "include/wrapper/cef_library_loader.h"
#include "host.h"
int main(int argc,char** argv) {
  // Since CEF M138 the sandbox dylib must initialize before loading CEF.
  CefScopedSandboxContext sandbox;
  if(!sandbox.Initialize(argc,argv)) return 1;
  CefScopedLibraryLoader loader;
  if(!loader.LoadInHelper()) return 1;
  return CefExecuteProcess(CefMainArgs(argc,argv),MakeRendererApp(),nullptr);
}
