#import <Cocoa/Cocoa.h>
#include "include/cef_application_mac.h"
#include "include/wrapper/cef_helpers.h"
#include "include/wrapper/cef_library_loader.h"
#include "host.h"
@interface QosApplication : NSApplication<CefAppProtocol> { BOOL handling_; }
@end
@implementation QosApplication
- (BOOL)isHandlingSendEvent { return handling_; }
- (void)setHandlingSendEvent:(BOOL)value { handling_=value; }
- (void)sendEvent:(NSEvent*)event { CefScopedSendingEvent scope; [super sendEvent:event]; }
- (void)terminate:(id)sender { CloseHost(); }
@end
int main(int argc,char** argv) {
  CefScopedLibraryLoader loader;
  if(!loader.LoadInMain()) return 1;
  @autoreleasepool {
    [QosApplication sharedApplication];
    [NSApp setActivationPolicy:NSApplicationActivationPolicyProhibited];
    return RunHost(argc,argv);
  }
}
