#import <AppKit/AppKit.h>
#import <Carbon/Carbon.h>
#include <stdio.h>

extern int pronto_carbon_hotkeys_start(void (*callback)(int, int));
extern int pronto_carbon_hotkey_update(int, UInt32, UInt32);
extern void pronto_carbon_hotkeys_stop(void);

static int pressed_count = 0;
static int released_count = 0;

static void received(int identifier, int pressed) {
    if (identifier != 1) return;
    if (pressed) pressed_count++;
    else released_count++;
}

static OSStatus send_hotkey(UInt32 kind, EventHotKeyID identifier) {
    EventRef event = NULL;
    OSStatus result = CreateEvent(NULL, kEventClassKeyboard, kind,
                                  GetCurrentEventTime(), 0, &event);
    if (result == noErr) {
        result = SetEventParameter(event, kEventParamDirectObject,
                                   typeEventHotKeyID, sizeof(identifier), &identifier);
    }
    if (result == noErr) result = SendEventToEventTarget(event, GetApplicationEventTarget());
    if (event) ReleaseEvent(event);
    return result;
}

int main(void) {
    [NSApplication sharedApplication];
    int started = pronto_carbon_hotkeys_start(received);
    int registered = pronto_carbon_hotkey_update(1, 7, controlKey);
    EventHotKeyID identifier = {'Prnt', 1};
    OSStatus down = send_hotkey(kEventHotKeyPressed, identifier);
    OSStatus up = send_hotkey(kEventHotKeyReleased, identifier);
    pronto_carbon_hotkeys_stop();
    printf("start=%d register=%d down=%d up=%d press=%d release=%d\n",
           started, registered, down, up, pressed_count, released_count);
    return !(started == noErr && registered == noErr && down == noErr &&
             up == noErr && pressed_count == 1 && released_count == 1);
}
