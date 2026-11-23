#import <Cocoa/Cocoa.h>
#import <Security/Security.h>
#import <CoreLocation/CoreLocation.h>
#include "geolocation.h"
#include <functional>
#include <algorithm>
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
// The person's own location, from the platform: what a page asking where it is
// gets when this machine will say. macOS answers only for an application the
// person has allowed to use Location Services, so a build running offscreen,
// headless or under a test account reports nothing and the page is told the
// position is unavailable, honestly.
// An allowed application's position arrives once; anything else (no
// authorization, Location Services off, a machine with no fix) answers nothing
// within the wait.
@interface TiloLocation : NSObject <CLLocationManagerDelegate>
@property(nonatomic, strong) CLLocationManager* manager;
// A C++ callback the Objective-C object owns and releases when it answers.
@property(nonatomic, assign) void* answerContext;
@property(nonatomic, assign) BOOL answered;
@end
static NSMutableArray<TiloLocation*>* tilo_locations;
@implementation TiloLocation
- (void)manager:(CLLocationManager*)manager didUpdateLocations:(NSArray<CLLocation*>*)locations {
  [self finish:[locations lastObject]];
}
- (void)manager:(CLLocationManager*)manager didFailWithError:(NSError*)error { [self finish:nil]; }

- (void)finish:(CLLocation*)location {
  if(self.answered) return;
  self.answered=YES;
  [self.manager stopUpdatingLocation];
  auto* context=static_cast<std::shared_ptr<std::function<void(std::optional<tilo::Position>)>>*>(self.answerContext);
  self.answerContext=nullptr;
  if(context) {
    auto held=std::move(*context);
    delete context;
    auto& answer=*held;
    if(location) {
      CLLocationCoordinate2D at=location.coordinate;
      tilo::Position position;
      position.latitude=at.latitude;position.longitude=at.longitude;
      position.accuracy=std::max(location.horizontalAccuracy,1.0);
      answer(position);
    } else {
      answer(std::nullopt);
    }
  }
  // The request is done with: it leaves the list that keeps it alive.
  if(tilo_locations) [tilo_locations removeObject:self];
}
@end
namespace tilo {
void SystemPosition(std::function<void(std::optional<Position>)> answer) {
  if(![CLLocationManager locationServicesEnabled]) { answer(std::nullopt);return; }
  if(!tilo_locations) tilo_locations=[NSMutableArray array];
  TiloLocation* request=[TiloLocation new];
  request.answered=NO;
  request.manager=[CLLocationManager new];
  request.manager.delegate=request;
  request.answerContext=new std::shared_ptr<std::function<void(std::optional<tilo::Position>)>>(
      std::make_shared<std::function<void(std::optional<tilo::Position>)>>(std::move(answer)));
  [tilo_locations addObject:request];
  // A machine that never answers (no authorization, Location Services off)
  // still leaves the page its honest answer, in time.
  dispatch_after(dispatch_time(DISPATCH_TIME_NOW,(int64_t)(5*NSEC_PER_SEC)),dispatch_get_main_queue(),^{
    [request finish:nil];
  });
  // Only a machine that already lets this application see its location answers
  // with one. Asking the person here would put a system dialog in front of
  // whoever is using the engine, which an offscreen or unattended engine never
  // does on its own: without that permission the page is told there is no
  // position, which is the truth.
  const auto status=request.manager.authorizationStatus;
  if(status==kCLAuthorizationStatusAuthorized || status==kCLAuthorizationStatusAuthorizedAlways) [request.manager requestLocation];
  else [request finish:nil];
}
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
