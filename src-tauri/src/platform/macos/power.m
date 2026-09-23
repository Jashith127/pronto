#import <AppKit/AppKit.h>

typedef void (*ProntoWakeCallback)(void);
static id wakeObserver;

void pronto_observe_wake(ProntoWakeCallback callback) {
    if (wakeObserver) return;
    wakeObserver = [[[NSWorkspace sharedWorkspace] notificationCenter]
        addObserverForName:NSWorkspaceDidWakeNotification object:nil queue:[NSOperationQueue mainQueue]
        usingBlock:^(NSNotification *notification) {
            (void)notification;
            callback();
        }];
}
