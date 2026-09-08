// ScreenCaptureKit bridge. Only audio outputs are registered: screen pixels are
// never delivered, retained or written. Called solely after explicit UI start.
#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreAudio/CoreAudioTypes.h>
#include <math.h>
#include <stdint.h>

typedef void (*JarvisAudioCallback)(void *, const float *, size_t, uint64_t, uint32_t);
typedef int (*JarvisCancelCallback)(void *);

static void copyError(NSString *error, char *buffer, size_t capacity) {
    if (capacity == 0 || buffer == NULL) return;
    snprintf(buffer, capacity, "%s", error.UTF8String ?: "Не удалось записать системный звук");
}

uint64_t jarvis_system_audio_host_time_ns(void) {
    CMTime time = CMClockGetTime(CMClockGetHostTimeClock());
    return (uint64_t)(CMTimeGetSeconds(time) * 1e9);
}

int jarvis_system_audio_available(void) {
    if (@available(macOS 13.0, *)) return NSClassFromString(@"SCStream") != nil;
    return 0;
}

API_AVAILABLE(macos(13.0))
@interface JarvisMeetingAudio : NSObject <SCStreamOutput, SCStreamDelegate>
@property(nonatomic, strong) SCStream *stream;
@property(nonatomic, strong) NSString *error;
@property(nonatomic) BOOL closed;
@property(nonatomic) JarvisAudioCallback callback;
@property(nonatomic) JarvisCancelCallback cancelled;
@property(nonatomic) void *context;
@property(nonatomic, strong) dispatch_queue_t queue;
- (void)close;
@end

@implementation JarvisMeetingAudio
- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
    @synchronized(self) {
        if (!self.closed) self.error = error.localizedDescription;
    }
}

- (void)stream:(SCStream *)stream didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer ofType:(SCStreamOutputType)type {
    if (type != SCStreamOutputTypeAudio || !CMSampleBufferDataIsReady(sampleBuffer)) return;
    @synchronized(self) {
        if (self.closed || (self.cancelled && self.cancelled(self.context))) return;
        const AudioStreamBasicDescription *format = CMAudioFormatDescriptionGetStreamBasicDescription(CMSampleBufferGetFormatDescription(sampleBuffer));
        if (format == NULL || format->mFormatID != kAudioFormatLinearPCM ||
            !(format->mFormatFlags & kAudioFormatFlagIsFloat) || format->mBitsPerChannel != 32 ||
            format->mChannelsPerFrame != 1 || format->mSampleRate != 16000) {
            self.error = @"ScreenCaptureKit вернул неподдерживаемый формат аудио";
            return;
        }
        AudioBufferList buffers;
        CMBlockBufferRef retainedBlock = NULL;
        OSStatus status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sampleBuffer, NULL, &buffers, sizeof(buffers), kCFAllocatorDefault,
            kCFAllocatorDefault, 0, &retainedBlock);
        if (status != noErr || buffers.mNumberBuffers != 1) {
            if (retainedBlock) CFRelease(retainedBlock);
            self.error = @"Не удалось прочитать системный аудиопоток";
            return;
        }
        const float *samples = (const float *)buffers.mBuffers[0].mData;
        size_t count = buffers.mBuffers[0].mDataByteSize / sizeof(float);
        if (samples != NULL && count > 0) {
            CMTime pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer);
            double seconds = CMTimeGetSeconds(pts);
            uint64_t time = CMTIME_IS_VALID(pts) && isfinite(seconds) && seconds >= 0
                ? (uint64_t)(seconds * 1e9)
                : jarvis_system_audio_host_time_ns() - (uint64_t)(count * 1e9 / format->mSampleRate);
            // Rust copies into a bounded nonblocking channel before this call
            // returns. The CMSampleBuffer never escapes its callback lifetime.
            self.callback(self.context, samples, count, time, (uint32_t)format->mSampleRate);
        }
        if (retainedBlock) CFRelease(retainedBlock);
    }
}

- (void)close {
    SCStream *stream;
    @synchronized(self) {
        self.closed = YES; // joins any in-flight callback before Rust frees context
        stream = self.stream;
    }
    if (stream) {
        dispatch_semaphore_t stopped = dispatch_semaphore_create(0);
        [stream stopCaptureWithCompletionHandler:^(NSError *error) {
            if (!error) {
                @synchronized(self) { if (self.stream == stream) self.stream = nil; }
            }
            dispatch_semaphore_signal(stopped);
        }];
        dispatch_semaphore_wait(stopped, dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC));
    }
}
@end

void *jarvis_system_audio_start(JarvisAudioCallback callback, JarvisCancelCallback cancelled, void *context, char *errorBuffer, size_t errorCapacity) {
    if (cancelled(context)) {
        copyError(@"Запуск записи отменён", errorBuffer, errorCapacity);
        return NULL;
    }
    if (@available(macOS 13.0, *)) {
        @autoreleasepool {
            JarvisMeetingAudio *capture = [JarvisMeetingAudio new];
            capture.callback = callback;
            capture.cancelled = cancelled;
            capture.context = context;
            capture.queue = dispatch_queue_create("app.jarvis.meeting-system-audio", DISPATCH_QUEUE_SERIAL);
            dispatch_semaphore_t ready = dispatch_semaphore_create(0);
            // This is the sole permission-triggering call and only runs on an
            // explicit online-meeting start. Discovery/status never invokes it.
            [SCShareableContent getShareableContentExcludingDesktopWindows:YES onScreenWindowsOnly:NO completionHandler:^(SCShareableContent *content, NSError *error) {
                @synchronized(capture) {
                    if (capture.closed) { dispatch_semaphore_signal(ready); return; }
                    if (capture.cancelled(capture.context)) {
                        capture.error = @"Запуск записи отменён";
                        capture.closed = YES;
                        dispatch_semaphore_signal(ready);
                        return;
                    }
                    if (error || content.displays.count == 0) {
                        capture.error = error.localizedDescription ?: @"Нет дисплея для захвата системного звука";
                        dispatch_semaphore_signal(ready);
                        return;
                    }
                    NSMutableArray<SCRunningApplication *> *excluded = [NSMutableArray array];
                    for (SCRunningApplication *app in content.applications) {
                        if (app.processID == NSProcessInfo.processInfo.processIdentifier) [excluded addObject:app];
                    }
                    SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:content.displays.firstObject excludingApplications:excluded exceptingWindows:@[]];
                    SCStreamConfiguration *config = [SCStreamConfiguration new];
                    config.width = 2;
                    config.height = 2;
                    config.minimumFrameInterval = CMTimeMake(1, 1);
                    config.showsCursor = NO;
                    config.capturesAudio = YES;
                    config.excludesCurrentProcessAudio = YES;
                    config.sampleRate = 16000;
                    config.channelCount = 1;
                    capture.stream = [[SCStream alloc] initWithFilter:filter configuration:config delegate:capture];
                    NSError *outputError = nil;
                    if (![capture.stream addStreamOutput:capture type:SCStreamOutputTypeAudio sampleHandlerQueue:capture.queue error:&outputError]) {
                        capture.error = outputError.localizedDescription ?: @"Не удалось подключить системный аудиопоток";
                        dispatch_semaphore_signal(ready);
                        return;
                    }
                    [capture.stream startCaptureWithCompletionHandler:^(NSError *startError) {
                        @synchronized(capture) {
                            if (startError) capture.error = startError.localizedDescription;
                            if (!capture.closed && capture.cancelled(capture.context)) {
                                capture.closed = YES;
                                capture.error = @"Запуск записи отменён";
                            }
                            if (capture.closed) [capture.stream stopCaptureWithCompletionHandler:^(NSError *ignored) {
                                @synchronized(capture) { capture.stream = nil; }
                            }];
                        }
                        dispatch_semaphore_signal(ready);
                    }];
                }
            }];
            uint64_t deadline = jarvis_system_audio_host_time_ns() + 7 * NSEC_PER_SEC;
            BOOL wasCancelled = NO;
            long timedOut;
            do {
                timedOut = dispatch_semaphore_wait(ready, dispatch_time(DISPATCH_TIME_NOW, 50 * NSEC_PER_MSEC));
                wasCancelled = cancelled(context);
            } while (timedOut && !wasCancelled && jarvis_system_audio_host_time_ns() < deadline);
            NSString *failure;
            @synchronized(capture) {
                failure = wasCancelled ? @"Запуск записи отменён" : timedOut ? @"Ожидание доступа к системному звуку истекло. Разрешите запись экрана и системного аудио в Системных настройках и повторите запуск." : capture.error;
            }
            if (failure) {
                [capture close];
                copyError(failure, errorBuffer, errorCapacity);
                return NULL;
            }
            return (__bridge_retained void *)capture;
        }
    }
    copyError(@"Запись системного звука требует macOS 13 или новее", errorBuffer, errorCapacity);
    return NULL;
}

int jarvis_system_audio_error(void *handle, char *buffer, size_t capacity) {
    if (@available(macOS 13.0, *)) {
        JarvisMeetingAudio *capture = (__bridge JarvisMeetingAudio *)handle;
        @synchronized(capture) {
            if (capture.error) { copyError(capture.error, buffer, capacity); return 1; }
        }
    }
    return 0;
}

void jarvis_system_audio_stop(void *handle) {
    if (@available(macOS 13.0, *)) {
        JarvisMeetingAudio *capture = (__bridge_transfer JarvisMeetingAudio *)handle;
        [capture close];
    }
}
