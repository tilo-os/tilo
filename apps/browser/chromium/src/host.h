#pragma once
#include "include/cef_app.h"
CefRefPtr<CefApp> MakeHostApp();
CefRefPtr<CefApp> MakeRendererApp();
int RunHost(int argc, char** argv);
void CloseHost();

#if defined(OS_MAC)
#include "include/cef_dialog_handler.h"
bool ShowHostFileDialog(cef_file_dialog_mode_t mode, const CefString& title, const std::vector<CefString>& extensions, CefRefPtr<CefFileDialogCallback> callback);
// CEF stamps a key event with the time of the Cocoa event being handled, and
// with 0 when there is none, as for every key the host forwards: a page then
// sees each press at the same instant and may take a second for the first
// repeated. While `now` is true, the host's keys are stamped with the time.
void StampHostKeys(bool now);
// Whether this engine is signed with a team identity (Developer ID): one identity
// across builds, so the login keychain's per-application permission holds.
bool HostHasStableSigningIdentity();
#endif
