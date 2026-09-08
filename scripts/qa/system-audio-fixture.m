// Native provider regression fixture. Includes the exact production delegate;
// creates synthetic CoreMedia sample buffers only. No SCStream is instantiated,
// no permission is requested, and no screen or microphone is captured.
#import "../../src-tauri/src/meetings/system_audio.m"
#include <stdatomic.h>

// Production never reads the stream argument. Passing nil deliberately avoids
// constructing a capture-capable SCStream for these sample-buffer fixtures.
#pragma clang diagnostic ignored "-Wnonnull"

#define CHECK(condition, message) do { if (!(condition)) { fprintf(stderr, "FAIL line %d: %s\n", __LINE__, message); exit(1); } } while (0)

typedef struct {
    size_t calls;
    size_t count;
    uint64_t time;
    uint32_t rate;
    float first;
    float last;
    atomic_bool cancelled;
    dispatch_semaphore_t entered;
    dispatch_semaphore_t release;
} Fixture;

static int fixtureCancelled(void *context) {
    return atomic_load(&((Fixture *)context)->cancelled);
}

static void fixtureAudio(void *context, const float *samples, size_t count, uint64_t time, uint32_t rate) {
    Fixture *fixture = context;
    fixture->calls++;
    fixture->count = count;
    fixture->time = time;
    fixture->rate = rate;
    fixture->first = samples[0];
    fixture->last = samples[count - 1];
    if (fixture->entered) {
        dispatch_semaphore_signal(fixture->entered);
        CHECK(dispatch_semaphore_wait(fixture->release, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC)) == 0,
            "fixture callback release timed out");
    }
}

static CMSampleBufferRef sampleBuffer(uint32_t rate) {
    float pcm[320];
    for (size_t i = 0; i < 320; i++) pcm[i] = (float)i / 640.0f;
    AudioStreamBasicDescription format = {
        .mSampleRate = rate, .mFormatID = kAudioFormatLinearPCM,
        .mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        .mBytesPerPacket = 4, .mFramesPerPacket = 1,
        .mBytesPerFrame = 4, .mChannelsPerFrame = 1, .mBitsPerChannel = 32,
    };
    CMAudioFormatDescriptionRef description = NULL;
    CHECK(CMAudioFormatDescriptionCreate(kCFAllocatorDefault, &format, 0, NULL, 0, NULL, NULL, &description) == noErr,
        "create PCM format description");
    CMBlockBufferRef block = NULL;
    CHECK(CMBlockBufferCreateWithMemoryBlock(kCFAllocatorDefault, NULL, sizeof(pcm), kCFAllocatorDefault,
        NULL, 0, sizeof(pcm), 0, &block) == noErr, "create PCM block");
    CHECK(CMBlockBufferReplaceDataBytes(pcm, block, 0, sizeof(pcm)) == noErr, "populate PCM block");
    CMSampleTimingInfo timing = { .duration = CMTimeMake(1, rate),
        .presentationTimeStamp = CMTimeMake(50, 1), .decodeTimeStamp = kCMTimeInvalid };
    size_t sampleSize = sizeof(float);
    CMSampleBufferRef sample = NULL;
    CHECK(CMSampleBufferCreateReady(kCFAllocatorDefault, block, description, 320, 1, &timing,
        1, &sampleSize, &sample) == noErr, "create ready PCM samples");
    CFRelease(block);
    CFRelease(description);
    return sample;
}

int main(void) {
    @autoreleasepool {
        if (@available(macOS 13.0, *)) {
            Fixture fixture = {0};
            atomic_init(&fixture.cancelled, false);
            JarvisMeetingAudio *capture = [JarvisMeetingAudio new];
            capture.callback = fixtureAudio;
            capture.cancelled = fixtureCancelled;
            capture.context = &fixture;
            CMSampleBufferRef valid = sampleBuffer(16000);

            [capture stream:nil didOutputSampleBuffer:valid ofType:SCStreamOutputTypeAudio];
            CHECK(fixture.calls == 1 && fixture.count == 320 && fixture.rate == 16000, "valid PCM callback shape");
            CHECK(fixture.time == 50000000000ULL, "presentation timestamp preserved");
            CHECK(fixture.first == 0.0f && fabsf(fixture.last - 319.0f / 640.0f) < 0.000001f, "PCM sample content preserved");
            CHECK(capture.error == nil, "valid PCM has no error");

            [capture stream:nil didOutputSampleBuffer:valid ofType:SCStreamOutputTypeScreen];
            CHECK(fixture.calls == 1, "screen sample is ignored");
            atomic_store(&fixture.cancelled, true);
            [capture stream:nil didOutputSampleBuffer:valid ofType:SCStreamOutputTypeAudio];
            CHECK(fixture.calls == 1, "cancelled callback does not deliver PCM");
            atomic_store(&fixture.cancelled, false);

            CMSampleBufferRef wrongRate = sampleBuffer(48000);
            [capture stream:nil didOutputSampleBuffer:wrongRate ofType:SCStreamOutputTypeAudio];
            CHECK(capture.error != nil && fixture.calls == 1, "unsupported format reports an error without data");
            CFRelease(wrongRate);
            capture.error = nil;

            // Real Objective-C monitor synchronization: close must wait for an
            // in-flight callback before the Rust-owned context may be released.
            fixture.entered = dispatch_semaphore_create(0);
            fixture.release = dispatch_semaphore_create(0);
            dispatch_semaphore_t closed = dispatch_semaphore_create(0);
            dispatch_semaphore_t closingStarted = dispatch_semaphore_create(0);
            dispatch_semaphore_t callbackDone = dispatch_semaphore_create(0);
            dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
                [capture stream:nil didOutputSampleBuffer:valid ofType:SCStreamOutputTypeAudio];
                dispatch_semaphore_signal(callbackDone);
            });
            CHECK(dispatch_semaphore_wait(fixture.entered, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC)) == 0,
                "fixture callback starts");
            dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
                dispatch_semaphore_signal(closingStarted);
                [capture close];
                dispatch_semaphore_signal(closed);
            });
            CHECK(dispatch_semaphore_wait(closingStarted, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC)) == 0,
                "close task is scheduled while callback is in flight");
            CHECK(dispatch_semaphore_wait(closed, dispatch_time(DISPATCH_TIME_NOW, 20 * NSEC_PER_MSEC)) != 0,
                "close cannot return while callback retains context");
            dispatch_semaphore_signal(fixture.release);
            CHECK(dispatch_semaphore_wait(callbackDone, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC)) == 0,
                "fixture callback completes");
            CHECK(dispatch_semaphore_wait(closed, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC)) == 0,
                "close completes after callback");
            fixture.entered = nil;
            fixture.release = nil;
            [capture stream:nil didOutputSampleBuffer:valid ofType:SCStreamOutputTypeAudio];
            CHECK(fixture.calls == 2, "closed provider cannot deliver a late sample");
            CFRelease(valid);
            puts("PASS: native CoreMedia PCM/content/PTS, unsupported format, cancellation, non-audio rejection, synchronized close, late callback suppression. Synthetic data only; no capture or permission request.");
            return 0;
        }
        fputs("SKIP: requires macOS 13 or newer\n", stderr);
        return 77;
    }
}
