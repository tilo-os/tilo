#include "host.h"
int main(int argc,char** argv) {
  int code=CefExecuteProcess(CefMainArgs(argc,argv),MakeRendererApp(),nullptr);
  if(code>=0) return code;
  return RunHost(argc,argv);
}
