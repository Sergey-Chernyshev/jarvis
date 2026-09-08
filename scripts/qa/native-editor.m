// A disposable two-field AppKit editor for real AX/CGEvent insertion tests.
// Clipboard data is retained only in memory and restored if no other app copied.
#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#include <signal.h>
#include <unistd.h>

@interface QAEditor : NSObject <NSApplicationDelegate, NSWindowDelegate>
@property NSString *root;
@property NSWindow *window;
@property NSTextView *fieldA;
@property NSTextView *fieldB;
@property NSArray<NSPasteboardItem *> *clipboard;
@property NSInteger clipboardGeneration;
@property NSString *lastCommand;
@property NSDate *started;
@property BOOL restoring;
@property BOOL stopping;
@property BOOL fullscreenEntered;
@property NSString *fullscreenTransition;
@property NSMutableArray<NSString *> *fullscreenEvents;
@property BOOL monitoring;
@property NSUInteger monitoredSamples;
@property NSMutableArray<NSDictionary *> *violations;
@property NSMutableArray<NSDictionary *> *workspaceEvents;
@property NSMutableArray *workspaceObservers;
@end
@implementation QAEditor
- (void)applicationDidFinishLaunching:(NSNotification *)note {
    NSPasteboard *pb=NSPasteboard.generalPasteboard;
    NSMutableArray *items=[NSMutableArray array];
    for(NSPasteboardItem *item in pb.pasteboardItems) {
        NSPasteboardItem *copy=[NSPasteboardItem new];
        for(NSPasteboardType type in item.types) { NSData *data=[item dataForType:type]; if(data) [copy setData:data forType:type]; }
        [items addObject:copy];
    }
    self.clipboard=items; self.clipboardGeneration=-1; self.started=NSDate.date;
    self.fullscreenEvents=[NSMutableArray array];self.violations=[NSMutableArray array];
    self.workspaceEvents=[NSMutableArray array];self.workspaceObservers=[NSMutableArray array];
    NSMenu *menu=[NSMenu new]; NSMenuItem *edit=[NSMenuItem new]; NSMenu *submenu=[[NSMenu alloc]initWithTitle:@"Edit"];
    [submenu addItemWithTitle:@"Paste" action:@selector(paste:) keyEquivalent:@"v"];
    edit.submenu=submenu;[menu addItem:edit];NSApp.mainMenu=menu;
    self.window=[[NSWindow alloc]initWithContentRect:NSMakeRect(200,200,640,440) styleMask:NSWindowStyleMaskTitled|NSWindowStyleMaskClosable|NSWindowStyleMaskResizable backing:NSBackingStoreBuffered defer:NO];
    self.window.delegate=self;
    self.window.collectionBehavior=NSWindowCollectionBehaviorFullScreenPrimary;
    self.window.title=@"Jarvis · изолированная проверка вставки";
    NSTextField *label=[NSTextField labelWithString:@"Автоматическая проверка Jarvis · окно закроется само"];
    label.autoresizingMask=NSViewMinYMargin|NSViewWidthSizable;
    label.frame=NSMakeRect(24,390,590,24);[self.window.contentView addSubview:label];
    self.fieldA=[[NSTextView alloc]initWithFrame:NSMakeRect(24,220,592,145)];
    self.fieldB=[[NSTextView alloc]initWithFrame:NSMakeRect(24,45,592,145)];
    for(NSTextView *field in @[self.fieldA,self.fieldB]) { field.font=[NSFont systemFontOfSize:16]; field.richText=NO; [self.window.contentView addSubview:field]; }
    self.fieldA.accessibilityLabel=@"Jarvis QA field A";self.fieldB.accessibilityLabel=@"Jarvis QA field B";
    __weak QAEditor *weakSelf=self;
    NSNotificationCenter *center=NSWorkspace.sharedWorkspace.notificationCenter;
    for(NSNotificationName name in @[NSWorkspaceDidActivateApplicationNotification,NSWorkspaceActiveSpaceDidChangeNotification]) {
        id observer=[center addObserverForName:name object:nil queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *notification) {
            [weakSelf recordWorkspaceEvent:[notification.name isEqualToString:NSWorkspaceDidActivateApplicationNotification]?@"application-activated":@"active-space-changed"];
        }];
        [self.workspaceObservers addObject:observer];
    }
    [self.window center];[self.window makeKeyAndOrderFront:nil];[NSApp activateIgnoringOtherApps:YES];[self.window makeFirstResponder:self.fieldA];
    [NSTimer scheduledTimerWithTimeInterval:.04 target:self selector:@selector(tick:) userInfo:nil repeats:YES];
    [self snapshot];
}
- (void)recordWorkspaceEvent:(NSString *)kind {
    if(self.workspaceEvents.count>=200)return;
    pid_t foreground=NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier;
    [self.workspaceEvents addObject:@{@"event":kind,@"timestampMs":@(NSDate.date.timeIntervalSince1970*1000),
        @"elapsedMs":@(-self.started.timeIntervalSinceNow*1000),@"foregroundPid":@(foreground),@"ownsForeground":@(foreground==getpid()),
        @"focused":@(NSApp.isActive),@"onActiveSpace":@(self.window.isOnActiveSpace),@"key":@(self.window.isKeyWindow),
        @"monitoring":@(self.monitoring),@"sample":@(self.monitoredSamples)}];
}
- (void)snapshot {
    pid_t foreground=NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier;
    BOOL fullscreen=(self.window.styleMask & NSWindowStyleMaskFullScreen)!=0;
    BOOL activeSpace=self.window.isOnActiveSpace;
    if(self.monitoring) {
        self.monitoredSamples++;
        if((foreground!=getpid() || !NSApp.isActive || !activeSpace || !fullscreen || !self.window.isKeyWindow) && self.violations.count<50)
            [self.violations addObject:@{@"sample":@(self.monitoredSamples),@"timestampMs":@(NSDate.date.timeIntervalSince1970*1000),@"foregroundPid":@(foreground),@"focused":@(NSApp.isActive),@"activeSpace":@(activeSpace),@"fullscreen":@(fullscreen),@"key":@(self.window.isKeyWindow)}];
    }
    // WindowServer metadata only; no screen pixels, titles, or user windows are
    // returned. The parent is the owned, disposable Jarvis process.
    NSArray *windows=CFBridgingRelease(CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly|kCGWindowListExcludeDesktopElements,kCGNullWindowID));
    NSMutableArray *ownedWindows=[NSMutableArray array];NSUInteger order=0;
    for(NSDictionary *entry in windows) {
        pid_t owner=[entry[(__bridge NSString *)kCGWindowOwnerPID] intValue];
        if(owner==getpid() || owner==getppid()) [ownedWindows addObject:@{
            @"pid":@(owner),@"number":entry[(__bridge NSString *)kCGWindowNumber]?:@0,
            @"layer":entry[(__bridge NSString *)kCGWindowLayer]?:@0,
            @"alpha":entry[(__bridge NSString *)kCGWindowAlpha]?:@0,
            @"bounds":entry[(__bridge NSString *)kCGWindowBounds]?:@{},@"frontToBackOrder":@(order)}];
        order++;
    }
    NSRect frame=self.window.frame,screen=self.window.screen.frame;
    NSEdgeInsets safe=NSEdgeInsetsMake(0,0,0,0);
    if(@available(macOS 12.0,*)) safe=self.window.screen.safeAreaInsets;
    CGDirectDisplayID display=[self.window.screen.deviceDescription[@"NSScreenNumber"] unsignedIntValue];
    CGRect displayBounds=CGDisplayBounds(display);
    CGRect expectedServerFrame=CGRectMake(displayBounds.origin.x+frame.origin.x-screen.origin.x,
        displayBounds.origin.y+screen.size.height-(frame.origin.y-screen.origin.y)-frame.size.height,frame.size.width,frame.size.height);
    NSDictionary *serverWindow=nil;
    for(NSDictionary *entry in ownedWindows) if([entry[@"number"] integerValue]==self.window.windowNumber) {serverWindow=entry;break;}
    CGRect serverFrame=CGRectZero;
    BOOL serverFrameAvailable=serverWindow && CGRectMakeWithDictionaryRepresentation((__bridge CFDictionaryRef)serverWindow[@"bounds"],&serverFrame);
    BOOL serverFrameStable=serverFrameAvailable && fabs(serverFrame.origin.x-expectedServerFrame.origin.x)<=2 && fabs(serverFrame.origin.y-expectedServerFrame.origin.y)<=2 && fabs(serverFrame.size.width-expectedServerFrame.size.width)<=2 && fabs(serverFrame.size.height-expectedServerFrame.size.height)<=2;
    // AppKit may keep isActive/isOnActiveSpace true while WindowServer is
    // sliding to another Space. Observe the actual owned fullscreen surface too.
    if(self.monitoring && !serverFrameStable && self.violations.count<50)
        [self.violations addObject:@{@"sample":@(self.monitoredSamples),@"timestampMs":@(NSDate.date.timeIntervalSince1970*1000),@"reason":@"owned fullscreen WindowServer surface moved or disappeared",@"bounds":serverWindow[@"bounds"]?:@{}}];
    NSDictionary *state=@{@"pid":@(getpid()),@"timestampMs":@(NSDate.date.timeIntervalSince1970*1000),@"workspaceEvents":self.workspaceEvents,
        @"a":self.fieldA.string?:@"",@"b":self.fieldB.string?:@"",@"command":self.lastCommand?:@"",@"focused":@(NSApp.isActive),
        @"foregroundPid":@(foreground),@"windowNumber":@(self.window.windowNumber),@"key":@(self.window.isKeyWindow),@"onActiveSpace":@(activeSpace),
        @"fullscreen":@(fullscreen),@"fullscreenEntered":@(self.fullscreenEntered),@"fullscreenTransition":self.fullscreenTransition?:@"",@"fullscreenEvents":self.fullscreenEvents,
        @"frame":@{@"x":@(frame.origin.x),@"y":@(frame.origin.y),@"width":@(frame.size.width),@"height":@(frame.size.height)},
        @"screenFrame":@{@"x":@(screen.origin.x),@"y":@(screen.origin.y),@"width":@(screen.size.width),@"height":@(screen.size.height)},
        @"screenSafeAreaFrame":@{@"x":@(screen.origin.x+safe.left),@"y":@(screen.origin.y+safe.bottom),@"width":@(screen.size.width-safe.left-safe.right),@"height":@(screen.size.height-safe.top-safe.bottom)},
        @"windowServerFrameStable":@(serverFrameStable),
        @"expectedWindowServerFrame":@{@"x":@(expectedServerFrame.origin.x),@"y":@(expectedServerFrame.origin.y),@"width":@(expectedServerFrame.size.width),@"height":@(expectedServerFrame.size.height)},
        @"monitoring":@(self.monitoring),@"monitoredSamples":@(self.monitoredSamples),@"violations":self.violations,@"ownedOnScreenWindows":ownedWindows};
    NSData *data=[NSJSONSerialization dataWithJSONObject:state options:0 error:nil];
    [data writeToFile:[self.root stringByAppendingPathComponent:@"editor-state.json"] atomically:YES];
}
- (void)tick:(NSTimer *)timer {
    NSPasteboard *pb=NSPasteboard.generalPasteboard;
    if([[pb stringForType:NSPasteboardTypeString] hasPrefix:@"Jarvis native paste QA"]) self.clipboardGeneration=pb.changeCount;
    NSString *command=[NSString stringWithContentsOfFile:[self.root stringByAppendingPathComponent:@"editor-command"] encoding:NSUTF8StringEncoding error:nil];
    if(command && ![command isEqualToString:self.lastCommand]) {
        self.lastCommand=command;
        if([command isEqualToString:@"stop"]) {[self stop];return;}
        if([command isEqualToString:@"focus-a"] || [command isEqualToString:@"focus-b"]) {
            [self.window makeKeyAndOrderFront:nil];[NSApp activateIgnoringOtherApps:YES];
            [self.window makeFirstResponder:[command isEqualToString:@"focus-a"]?self.fieldA:self.fieldB];
        }
        if([command isEqualToString:@"enter-fullscreen"] && !(self.window.styleMask & NSWindowStyleMaskFullScreen) && !self.fullscreenTransition.length) [self.window toggleFullScreen:nil];
        if([command isEqualToString:@"exit-fullscreen"] && (self.window.styleMask & NSWindowStyleMaskFullScreen) && !self.fullscreenTransition.length) [self.window toggleFullScreen:nil];
        if([command isEqualToString:@"monitor-on"]) { self.monitoredSamples=0;[self.violations removeAllObjects];self.monitoring=YES;[self recordWorkspaceEvent:@"monitoring-started"]; }
        if([command isEqualToString:@"monitor-off"]) {self.monitoring=NO;[self recordWorkspaceEvent:@"monitoring-stopped"];}
    }
    [self snapshot];
    if(-self.started.timeIntervalSinceNow>60 || getppid()==1) [self stop];
}
- (void)windowWillEnterFullScreen:(NSNotification *)note { self.fullscreenTransition=@"entering";[self.fullscreenEvents addObject:@"will-enter"]; }
- (void)windowDidEnterFullScreen:(NSNotification *)note { self.fullscreenTransition=@"";self.fullscreenEntered=YES;[self.fullscreenEvents addObject:@"did-enter"];if(self.stopping)[self stop]; }
- (void)windowWillExitFullScreen:(NSNotification *)note { self.fullscreenTransition=@"exiting";[self.fullscreenEvents addObject:@"will-exit"]; }
- (void)windowDidExitFullScreen:(NSNotification *)note { self.fullscreenTransition=@"";self.fullscreenEntered=NO;[self.fullscreenEvents addObject:@"did-exit"];if(self.stopping)[NSApp terminate:nil]; }
- (void)windowDidFailToEnterFullScreen:(NSWindow *)window { self.fullscreenTransition=@"";[self.fullscreenEvents addObject:@"failed-enter"];if(self.stopping)[NSApp terminate:nil]; }
- (void)windowDidFailToExitFullScreen:(NSWindow *)window { self.fullscreenTransition=@"";[self.fullscreenEvents addObject:@"failed-exit"];if(self.stopping)[NSApp terminate:nil]; }
- (void)stop {
    self.stopping=YES;self.monitoring=NO;
    if(self.fullscreenTransition.length)return;
    if(self.window.styleMask & NSWindowStyleMaskFullScreen) [self.window toggleFullScreen:nil];
    else [NSApp terminate:nil];
}
- (void)applicationWillTerminate:(NSNotification *)note {
    if(self.restoring)return;self.restoring=YES;
    for(id observer in self.workspaceObservers) [NSWorkspace.sharedWorkspace.notificationCenter removeObserver:observer];
    [self.workspaceObservers removeAllObjects];
    NSPasteboard *pb=NSPasteboard.generalPasteboard;
    if(self.clipboardGeneration>=0 && pb.changeCount==self.clipboardGeneration && [[pb stringForType:NSPasteboardTypeString] hasPrefix:@"Jarvis native paste QA"]) {
        [pb clearContents];if(self.clipboard.count)[pb writeObjects:self.clipboard];
    }
}
@end
int main(int argc,char **argv) { @autoreleasepool {
    if(argc!=2)return 2;
    [NSApplication sharedApplication];[NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
    QAEditor *app=[QAEditor new];app.root=[NSString stringWithUTF8String:argv[1]];NSApp.delegate=app;
    signal(SIGTERM,SIG_IGN);
    dispatch_source_t term=dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL,SIGTERM,0,dispatch_get_main_queue());
    dispatch_source_set_event_handler(term,^{[app stop];});dispatch_resume(term);
    [NSApp run];return 0;
} }
