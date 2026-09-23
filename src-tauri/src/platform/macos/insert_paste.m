#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <CoreGraphics/CoreGraphics.h>
#include <unistd.h>

static NSLock *captured_lock(void) {
    static NSLock *lock;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ lock = [NSLock new]; });
    return lock;
}

static AXUIElementRef captured_field;
static pid_t captured_pid;

void pronto_clear_captured_text_field(void) {
    NSLock *lock = captured_lock();
    [lock lock];
    if (captured_field) CFRelease(captured_field);
    captured_field = NULL;
    captured_pid = 0;
    [lock unlock];
}

static AXUIElementRef current_focused_field(pid_t pid) {
    AXUIElementRef application = AXUIElementCreateApplication(pid);
    AXUIElementSetMessagingTimeout(application, 0.25);
    AXUIElementRef focused = NULL;
    AXError error = AXUIElementCopyAttributeValue(application, kAXFocusedUIElementAttribute,
                                                   (CFTypeRef *)&focused);
    CFRelease(application);
    return error == kAXErrorSuccess ? focused : NULL;
}

static BOOL is_editable_field(AXUIElementRef element) {
    Boolean settable = false;
    if (AXUIElementIsAttributeSettable(element, kAXSelectedTextAttribute, &settable) == kAXErrorSuccess && settable) return YES;
    if (AXUIElementIsAttributeSettable(element, kAXValueAttribute, &settable) == kAXErrorSuccess && settable) return YES;
    CFTypeRef role = NULL;
    AXError error = AXUIElementCopyAttributeValue(element, kAXRoleAttribute, &role);
    BOOL editable = error == kAXErrorSuccess && role && CFGetTypeID(role) == CFStringGetTypeID() &&
        (CFEqual(role, kAXTextFieldRole) || CFEqual(role, kAXTextAreaRole) || CFEqual(role, kAXComboBoxRole));
    if (role) CFRelease(role);
    return editable;
}

int pronto_focused_text_field_is_editable(pid_t pid) {
    if (pid <= 0 || !AXIsProcessTrusted()) return 0;
    AXUIElementRef focused = current_focused_field(pid);
    if (!focused) return 0;
    AXUIElementSetMessagingTimeout(focused, 0.25);
    BOOL editable = is_editable_field(focused);
    CFRelease(focused);
    return editable;
}

int pronto_capture_focused_text_field(pid_t pid) {
    if (pid <= 0 || !AXIsProcessTrusted()) return 0;
    AXUIElementRef focused = current_focused_field(pid);
    if (!focused) return 0;
    AXUIElementSetMessagingTimeout(focused, 0.25);
    if (!is_editable_field(focused)) {
        CFRelease(focused);
        return 0;
    }
    NSLock *lock = captured_lock();
    [lock lock];
    if (captured_field) CFRelease(captured_field);
    captured_field = focused;
    captured_pid = pid;
    [lock unlock];
    return 1;
}

static AXUIElementRef copy_captured_field(pid_t pid) {
    NSLock *lock = captured_lock();
    [lock lock];
    AXUIElementRef field = captured_pid == pid && captured_field ? CFRetain(captured_field) : NULL;
    [lock unlock];
    return field;
}

static BOOL restore_captured_focus(pid_t pid, AXUIElementRef field) {
    NSRunningApplication *application = [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
    if (!application || (!application.active && ![application activateWithOptions:0])) return NO;
    AXUIElementRef app_element = AXUIElementCreateApplication(pid);
    AXUIElementSetMessagingTimeout(app_element, 0.25);
    BOOL focused = NO;
    for (int attempt = 0; attempt < 8 && !focused; attempt++) {
        AXUIElementSetAttributeValue(field, kAXFocusedAttribute, kCFBooleanTrue);
        AXUIElementSetAttributeValue(app_element, kAXFocusedUIElementAttribute, field);
        AXUIElementRef current = NULL;
        if (AXUIElementCopyAttributeValue(app_element, kAXFocusedUIElementAttribute,
                                          (CFTypeRef *)&current) == kAXErrorSuccess && current) {
            focused = CFEqual(current, field);
            CFRelease(current);
        }
        if (!focused) usleep(25000);
    }
    CFRelease(app_element);
    return focused;
}

static AXUIElementRef insertion_field(pid_t pid, BOOL use_captured) {
    AXUIElementRef field = use_captured ? copy_captured_field(pid) : NULL;
    if (field) {
        if (restore_captured_focus(pid, field)) return field;
        CFRelease(field);
    }
    return current_focused_field(pid);
}

static NSArray<NSPasteboardItem *> *snapshot_pasteboard(NSPasteboard *pasteboard) {
    NSMutableArray<NSPasteboardItem *> *snapshot = [NSMutableArray array];
    for (NSPasteboardItem *item in pasteboard.pasteboardItems ?: @[]) {
        NSPasteboardItem *copy = [NSPasteboardItem new];
        for (NSPasteboardType type in item.types) {
            NSData *data = [item dataForType:type];
            // Lazy or unsupported formats cannot be restored faithfully.
            if (!data || ![copy setData:data forType:type]) return nil;
        }
        [snapshot addObject:copy];
    }
    return snapshot;
}

static BOOL restore_pasteboard_if_unchanged(NSPasteboard *pasteboard,
                                            NSArray<NSPasteboardItem *> *snapshot,
                                            NSInteger temporary_change) {
    // An intervening copy belongs to the user or another app; preserve it.
    if (pasteboard.changeCount != temporary_change) return YES;
    [pasteboard clearContents];
    return snapshot.count == 0 || [pasteboard writeObjects:snapshot];
}

static NSString *focused_value(AXUIElementRef element) {
    CFTypeRef value = NULL;
    AXError error = AXUIElementCopyAttributeValue(element, kAXValueAttribute, &value);
    if (error != kAXErrorSuccess || !value) return nil;
    NSString *string = CFGetTypeID(value) == CFStringGetTypeID() ? [(__bridge NSString *)value copy] : nil;
    CFRelease(value);
    return string;
}

// 1 = verified, 2 = accepted but unreadable, 0 = rejected or unchanged.
int pronto_insert_with_accessibility(pid_t pid, const char *utf8, int use_captured) {
    @autoreleasepool {
        NSString *text = [NSString stringWithUTF8String:utf8];
        if (!text) return 0;
        AXUIElementRef focused = insertion_field(pid, use_captured != 0);
        if (!focused) return 0;
        NSString *before = focused_value(focused);
        AXError error = AXUIElementSetAttributeValue(focused, kAXSelectedTextAttribute,
                                                     (__bridge CFTypeRef)text);
        if (error != kAXErrorSuccess) {
            CFRelease(focused);
            return 0;
        }
        if (!before) {
            CFRelease(focused);
            return 2;
        }
        BOOL changed = NO;
        BOOL unreadable = NO;
        for (int attempt = 0; attempt < 12; attempt++) {
            NSString *after = focused_value(focused);
            if (!after) {
                unreadable = YES;
            } else if (![after isEqualToString:before]) {
                if ([after containsString:text]) {
                    CFRelease(focused);
                    return 1;
                }
                changed = YES;
            }
            usleep(25000);
        }
        CFRelease(focused);
        return changed || unreadable ? 2 : 0;
    }
}

// 1 = verified, 2 = paste sent but unverified, 0 = no paste event sent.
// The transcript is temporary pasteboard data; restore the prior contents
// even when delivery cannot be verified.
int pronto_insert_with_pasteboard(pid_t pid, const char *utf8, int use_captured) {
    @autoreleasepool {
        NSString *text = [NSString stringWithUTF8String:utf8];
        if (!text) return -1;
        NSPasteboard *pasteboard = NSPasteboard.generalPasteboard;
        const CGEventFlags heldModifiers = kCGEventFlagMaskControl | kCGEventFlagMaskAlternate |
                                           kCGEventFlagMaskCommand | kCGEventFlagMaskShift;
        BOOL released = NO;
        for (int attempt = 0; attempt < 30; attempt++) {
            if ((CGEventSourceFlagsState(kCGEventSourceStateCombinedSessionState) & heldModifiers) == 0) {
                released = YES;
                break;
            }
            usleep(25000);
        }
        if (!released) {
            return 0;
        }
        AXUIElementRef focused = insertion_field(pid, use_captured != 0);
        if (!focused) {
            return 0;
        }
        NSString *before = focused_value(focused);
        NSArray<NSPasteboardItem *> *snapshot = snapshot_pasteboard(pasteboard);
        if (!snapshot) {
            CFRelease(focused);
            return 0;
        }
        [pasteboard clearContents];
        NSInteger clearedChange = pasteboard.changeCount;
        if (![pasteboard setString:text forType:NSPasteboardTypeString]) {
            BOOL restored = restore_pasteboard_if_unchanged(pasteboard, snapshot, clearedChange);
            CFRelease(focused);
            return restored ? 0 : -2;
        }
        NSInteger temporaryChange = pasteboard.changeCount;
        CGEventRef down = CGEventCreateKeyboardEvent(NULL, 9, true); // V
        CGEventRef up = CGEventCreateKeyboardEvent(NULL, 9, false);
        if (!down || !up) {
            if (down) CFRelease(down);
            if (up) CFRelease(up);
            BOOL restored = restore_pasteboard_if_unchanged(pasteboard, snapshot, temporaryChange);
            CFRelease(focused);
            return restored ? 0 : -2;
        }
        CGEventSetFlags(down, kCGEventFlagMaskCommand);
        CGEventSetFlags(up, kCGEventFlagMaskCommand);
        CGEventPostToPid(pid, down);
        CGEventPostToPid(pid, up);
        CFRelease(down);
        CFRelease(up);
        BOOL delivered = NO;
        for (int attempt = 0; attempt < 20; attempt++) {
            usleep(25000);
            NSString *after = before ? focused_value(focused) : nil;
            if (after && ![after isEqualToString:before] && [after containsString:text]) {
                delivered = YES;
                break;
            }
        }
        CFRelease(focused);
        if (!restore_pasteboard_if_unchanged(pasteboard, snapshot, temporaryChange)) return -2;
        return delivered ? 1 : 2;
    }
}
