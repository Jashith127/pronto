#import <AppKit/AppKit.h>
#import <Carbon/Carbon.h>

typedef void (*ProntoHotkeyCallback)(int identifier, int pressed);

static ProntoHotkeyCallback callback;
static EventHandlerRef handler;
static EventHotKeyRef registrations[4];
static UInt32 savedKeys[4];
static UInt32 savedModifiers[4];

static OSStatus handle_hotkey(EventHandlerCallRef next, EventRef event, void *context) {
    (void)next;
    (void)context;
    EventHotKeyID hotkey = {0};
    if (GetEventParameter(event, kEventParamDirectObject, typeEventHotKeyID,
                          NULL, sizeof(hotkey), NULL, &hotkey) != noErr ||
        hotkey.signature != 'Prnt' || hotkey.id < 1 || hotkey.id > 3) {
        return eventNotHandledErr;
    }
    if (callback) callback((int)hotkey.id, GetEventKind(event) == kEventHotKeyPressed);
    return noErr;
}

static int start_on_main(ProntoHotkeyCallback next_callback) {
    callback = next_callback;
    if (handler) return noErr;
    EventTypeSpec types[] = {
        {kEventClassKeyboard, kEventHotKeyPressed},
        {kEventClassKeyboard, kEventHotKeyReleased},
    };
    return InstallEventHandler(GetApplicationEventTarget(), handle_hotkey, 2,
                               types, NULL, &handler);
}

int pronto_carbon_hotkeys_start(ProntoHotkeyCallback next_callback) {
    __block int result;
    if ([NSThread isMainThread]) result = start_on_main(next_callback);
    else dispatch_sync(dispatch_get_main_queue(), ^{ result = start_on_main(next_callback); });
    return result;
}

static int update_on_main(int identifier, UInt32 key, UInt32 modifiers) {
    if (!handler || identifier < 1 || identifier > 3) return paramErr;
    EventHotKeyRef previous = registrations[identifier];
    UInt32 previousKey = savedKeys[identifier];
    UInt32 previousModifiers = savedModifiers[identifier];
    if (previous) {
        UnregisterEventHotKey(previous);
        registrations[identifier] = NULL;
    }
    if (key == UINT32_MAX) return noErr;
    EventHotKeyID id = {'Prnt', (UInt32)identifier};
    OSStatus status = RegisterEventHotKey(key, modifiers, id,
                                          GetApplicationEventTarget(), 0,
                                          &registrations[identifier]);
    if (status == noErr) {
        savedKeys[identifier] = key;
        savedModifiers[identifier] = modifiers;
        return noErr;
    }
    if (previous) {
        EventHotKeyID restoreId = {'Prnt', (UInt32)identifier};
        RegisterEventHotKey(previousKey, previousModifiers, restoreId,
                            GetApplicationEventTarget(), 0,
                            &registrations[identifier]);
    }
    return status;
}

// UINT32_MAX removes a registration (modifier-only shortcuts use CGEventTap).
int pronto_carbon_hotkey_update(int identifier, UInt32 key, UInt32 modifiers) {
    __block int result;
    if ([NSThread isMainThread]) result = update_on_main(identifier, key, modifiers);
    else dispatch_sync(dispatch_get_main_queue(), ^{ result = update_on_main(identifier, key, modifiers); });
    return result;
}

void pronto_carbon_hotkeys_stop(void) {
    void (^stop)(void) = ^{
        for (int identifier = 1; identifier <= 3; identifier++) {
            if (registrations[identifier]) {
                UnregisterEventHotKey(registrations[identifier]);
                registrations[identifier] = NULL;
            }
        }
        if (handler) {
            RemoveEventHandler(handler);
            handler = NULL;
        }
        callback = NULL;
    };
    if ([NSThread isMainThread]) stop();
    else dispatch_sync(dispatch_get_main_queue(), stop);
}
