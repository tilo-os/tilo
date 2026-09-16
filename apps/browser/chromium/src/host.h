#pragma once
#include "include/cef_app.h"
CefRefPtr<CefApp> MakeHostApp();
CefRefPtr<CefApp> MakeRendererApp();
int RunHost(int argc, char** argv);
void CloseHost();
