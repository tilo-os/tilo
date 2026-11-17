#if os(iOS)
import UIKit

UIApplicationMain(CommandLine.argc, CommandLine.unsafeArgv, nil, NSStringFromClass(PhoneAppDelegate.self))
#else
print("TiloPhone is the iPhone app. The Mac app is TiloMac.")
#endif
