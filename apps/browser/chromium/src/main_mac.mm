#import <Cocoa/Cocoa.h>
#import <Security/Security.h>
#include "include/cef_application_mac.h"
#include "include/wrapper/cef_helpers.h"
#include "include/wrapper/cef_library_loader.h"
#include "host.h"
@interface TiloApplication : NSApplication<CefAppProtocol> { BOOL handling_; BOOL stamping_; }
- (void)setStampingKeys:(BOOL)value;
@end
@implementation TiloApplication
- (BOOL)isHandlingSendEvent { return handling_; }
- (void)setStampingKeys:(BOOL)value { stamping_=value; }
// A key the host forwards comes from no Cocoa event; CEF takes its time from
// this one (StampHostKeys).
- (NSEvent*)currentEvent {
  NSEvent* event=[super currentEvent];
  if(!stamping_ || event) return event;
  return [NSEvent otherEventWithType:NSEventTypeApplicationDefined location:NSZeroPoint modifierFlags:0
                           timestamp:[NSProcessInfo processInfo].systemUptime windowNumber:0 context:nil subtype:0 data1:0 data2:0];
}
- (void)setHandlingSendEvent:(BOOL)value { handling_=value; }
- (void)sendEvent:(NSEvent*)event { CefScopedSendingEvent scope; [super sendEvent:event]; }
- (void)terminate:(id)sender { CloseHost(); }
@end
void StampHostKeys(bool now) { [static_cast<TiloApplication*>(NSApp) setStampingKeys:now]; }
bool ShowHostFileDialog(cef_file_dialog_mode_t mode,const CefString& title,const std::vector<CefString>& extensions,CefRefPtr<CefFileDialogCallback> callback) {
  if(mode==FILE_DIALOG_SAVE) return false;
  NSOpenPanel* panel=[NSOpenPanel openPanel];
  panel.canChooseFiles=mode!=FILE_DIALOG_OPEN_FOLDER;
  panel.canChooseDirectories=mode==FILE_DIALOG_OPEN_FOLDER;
  panel.allowsMultipleSelection=mode==FILE_DIALOG_OPEN_MULTIPLE;
  if(!title.empty()) panel.title=[NSString stringWithUTF8String:title.ToString().c_str()];
  NSMutableArray<NSString*>* types=[NSMutableArray array];
  for(const auto& extension:extensions) {
    NSString* group=[NSString stringWithUTF8String:extension.ToString().c_str()];
    for(NSString* entry in [group componentsSeparatedByString:@";"]) {
      NSString* value=[entry stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceCharacterSet]];
      if([value hasPrefix:@"."]) value=[value substringFromIndex:1];
      if(value.length && ![value isEqualToString:@"*"] && ![types containsObject:value]) [types addObject:value];
    }
  }
  if(types.count) panel.allowedFileTypes=types;
  NSRunningApplication* previous=[[NSWorkspace sharedWorkspace] frontmostApplication];
  [panel beginWithCompletionHandler:^(NSModalResponse result) {
    if(result==NSModalResponseOK) { std::vector<CefString> paths;for(NSURL* url in panel.URLs) paths.emplace_back(url.path.UTF8String);callback->Continue(paths); }
    else callback->Cancel();
    [previous activateWithOptions:NSApplicationActivateIgnoringOtherApps];
  }];
  [NSApp activateIgnoringOtherApps:YES];[panel makeKeyAndOrderFront:nil];
  return true;
}
bool HostHasStableSigningIdentity() {
  SecCodeRef self=nullptr;
  if(SecCodeCopySelf(kSecCSDefaultFlags,&self)!=errSecSuccess||!self) return false;
  SecStaticCodeRef code=nullptr;
  CFDictionaryRef info=nullptr;
  bool team=false;
  if(SecCodeCopyStaticCode(self,kSecCSDefaultFlags,&code)==errSecSuccess&&code&&
     SecCodeCopySigningInformation(code,kSecCSSigningInformation,&info)==errSecSuccess&&info) {
    CFTypeRef id=CFDictionaryGetValue(info,kSecCodeInfoTeamIdentifier);
    team=id&&CFGetTypeID(id)==CFStringGetTypeID()&&CFStringGetLength((CFStringRef)id)>0;
  }
  if(info) CFRelease(info);
  if(code) CFRelease(code);
  CFRelease(self);
  return team;
}
int main(int argc,char** argv) {
  CefScopedLibraryLoader loader;
  if(!loader.LoadInMain()) return 1;
  @autoreleasepool {
    [TiloApplication sharedApplication];
    [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
    return RunHost(argc,argv);
  }
}
