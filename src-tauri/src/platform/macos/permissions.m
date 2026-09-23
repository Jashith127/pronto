#import <AppKit/AppKit.h>
#import <AVFoundation/AVFoundation.h>
#import <ApplicationServices/ApplicationServices.h>
#import <CoreGraphics/CoreGraphics.h>

// 0 microphone, 1 Accessibility, 2 Input Monitoring, 3 Screen Recording.
int pronto_permission_status(int kind) {
    switch (kind) {
        case 0: return [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio] == AVAuthorizationStatusAuthorized;
        case 1: return AXIsProcessTrusted();
        case 2: return CGPreflightListenEventAccess();
        case 3: return CGPreflightScreenCaptureAccess();
        default: return 0;
    }
}

void pronto_request_permission(int kind) {
    switch (kind) {
        case 0:
            [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio completionHandler:^(BOOL granted) { (void)granted; }];
            break;
        case 1: {
            NSDictionary *options = @{ (__bridge NSString *)kAXTrustedCheckOptionPrompt: @YES };
            AXIsProcessTrustedWithOptions((__bridge CFDictionaryRef)options);
            break;
        }
        case 2: CGRequestListenEventAccess(); break;
        case 3: CGRequestScreenCaptureAccess(); break;
    }
}

void pronto_open_permission_settings(int kind) {
    NSArray<NSString *> *panes = @[@"Privacy_Microphone", @"Privacy_Accessibility",
                                    @"Privacy_ListenEvent", @"Privacy_ScreenCapture"];
    if (kind < 0 || kind >= (int)panes.count) return;
    NSString *address = [@"x-apple.systempreferences:com.apple.preference.security?" stringByAppendingString:panes[kind]];
    dispatch_async(dispatch_get_main_queue(), ^{
        [[NSWorkspace sharedWorkspace] openURL:[NSURL URLWithString:address]];
    });
}
