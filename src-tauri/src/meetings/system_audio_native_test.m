// Synthetic CoreMedia tests. This executable does not call an uncancelled
// start, request permissions, enumerate displays, or capture live user audio.
// Run from src-tauri:
// xcrun clang -fobjc-arc -fblocks -mmacosx-version-min=11.0 \
//   src/meetings/system_audio_native_test.m -framework Foundation \
//   -framework CoreMedia -weak_framework ScreenCaptureKit -o /tmp/jarvis-audio-test
// /tmp/jarvis-audio-test
#import "system_audio.m"
#include <assert.h>

typedef struct { int calls; int cancel; } TestContext;
static int isCancelled(void *context) { return ((TestContext *)context)->cancel; }
static void receive(void *context, const float *pcm, size_t count, uint64_t time, uint32_t rate) {
    TestContext *state = context;
    state->calls += 1;
    assert(count == 5);
    assert(rate == 16000);
    assert(time == 123000000000ULL);
    assert(pcm[0] == 0.0 && pcm[1] == 0.25 && pcm[2] == -0.5 && pcm[4] == 0.75);
}

static CMSampleBufferRef sample(void) {
    float pcm[] = {0.0, 0.25, -0.5, 0.5, 0.75};
    AudioStreamBasicDescription asbd = {0};
    asbd.mSampleRate = 16000;
    asbd.mFormatID = kAudioFormatLinearPCM;
    asbd.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;
    asbd.mBytesPerPacket = sizeof(float);
    asbd.mFramesPerPacket = 1;
    asbd.mBytesPerFrame = sizeof(float);
    asbd.mChannelsPerFrame = 1;
    asbd.mBitsPerChannel = 32;
    CMAudioFormatDescriptionRef format = NULL;
    assert(CMAudioFormatDescriptionCreate(kCFAllocatorDefault, &asbd, 0, NULL, 0, NULL, NULL, &format) == noErr);
    CMBlockBufferRef block = NULL;
    assert(CMBlockBufferCreateWithMemoryBlock(kCFAllocatorDefault, NULL, sizeof(pcm), kCFAllocatorDefault, NULL, 0, sizeof(pcm), 0, &block) == noErr);
    assert(CMBlockBufferReplaceDataBytes(pcm, block, 0, sizeof(pcm)) == noErr);
    CMSampleTimingInfo timing = { CMTimeMake(1, 16000), CMTimeMake(123, 1), kCMTimeInvalid };
    CMSampleBufferRef result = NULL;
    assert(CMSampleBufferCreateReady(kCFAllocatorDefault, block, format, 5, 1, &timing, 0, NULL, &result) == noErr);
    CFRelease(block);
    CFRelease(format);
    return result;
}

int main(void) {
    @autoreleasepool {
        TestContext cancelled = {.calls = 0, .cancel = 1};
        char error[256] = {0};
        assert(jarvis_system_audio_start(receive, isCancelled, &cancelled, error, sizeof(error)) == NULL);
        assert(strstr(error, "отменён") != NULL);
        assert(cancelled.calls == 0);
        if (@available(macOS 13.0, *)) {
            JarvisMeetingAudio *capture = [JarvisMeetingAudio new];
            TestContext context = {.calls = 0, .cancel = 0};
            capture.context = &context;
            capture.callback = receive;
            capture.cancelled = isCancelled;
            CMSampleBufferRef buffer = sample();
            [capture stream:nil didOutputSampleBuffer:buffer ofType:SCStreamOutputTypeAudio];
            assert(context.calls == 1);
            assert(capture.error == nil);
            [capture stream:nil didOutputSampleBuffer:buffer ofType:SCStreamOutputTypeScreen];
            assert(context.calls == 1);
            context.cancel = 1;
            [capture stream:nil didOutputSampleBuffer:buffer ofType:SCStreamOutputTypeAudio];
            assert(context.calls == 1);
            capture.closed = YES;
            // Closed must short-circuit before touching the now-invalid context.
            capture.context = NULL;
            [capture stream:nil didOutputSampleBuffer:buffer ofType:SCStreamOutputTypeAudio];
            assert(context.calls == 1);
            CFRelease(buffer);
        }
        puts("Native synthetic audio tests passed (no live capture)");
    }
    return 0;
}
