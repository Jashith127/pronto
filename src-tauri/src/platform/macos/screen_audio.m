#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreAudio/CoreAudioTypes.h>
#import <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>

typedef bool (*ProntoShouldStop)(void *context);
typedef void (*ProntoAudioSamples)(const float *samples, size_t count, uint32_t rate, void *context);

@interface ProntoAudioOutput : NSObject <SCStreamOutput, SCStreamDelegate>
@property(nonatomic, assign) ProntoAudioSamples receive;
@property(nonatomic, assign) void *context;
@property(nonatomic, strong) NSError *stoppedError;
@end

@implementation ProntoAudioOutput
- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
    (void)stream;
    @synchronized(self) { self.stoppedError = error; }
}

- (void)stream:(SCStream *)stream didOutputSampleBuffer:(CMSampleBufferRef)sample ofType:(SCStreamOutputType)type {
    (void)stream;
    if (type != SCStreamOutputTypeAudio || !CMSampleBufferIsValid(sample)) return;
    CMAudioFormatDescriptionRef description = (CMAudioFormatDescriptionRef)CMSampleBufferGetFormatDescription(sample);
    if (!description) return;
    const AudioStreamBasicDescription *format = CMAudioFormatDescriptionGetStreamBasicDescription(description);
    if (!format || format->mFormatID != kAudioFormatLinearPCM || format->mSampleRate <= 0) return;
    size_t listSize = 0;
    OSStatus result = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sample, &listSize, NULL, 0, NULL, NULL, 0, NULL);
    if (result != noErr && listSize == 0) return;
    AudioBufferList *list = malloc(listSize);
    if (!list) return;
    CMBlockBufferRef block = NULL;
    result = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sample, NULL, list, listSize, NULL, NULL, 0, &block);
    if (result == noErr && list->mNumberBuffers > 0) {
        size_t frames = (size_t)CMSampleBufferGetNumSamples(sample);
        float *mono = malloc(frames * sizeof(float));
        if (mono) {
            const bool isFloat = (format->mFormatFlags & kAudioFormatFlagIsFloat) != 0 && format->mBitsPerChannel == 32;
            const bool isInt16 = (format->mFormatFlags & kAudioFormatFlagIsSignedInteger) != 0 && format->mBitsPerChannel == 16;
            const bool planar = (format->mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0;
            uint32_t channels = format->mChannelsPerFrame;
            if ((isFloat || isInt16) && channels > 0) {
                for (size_t frame = 0; frame < frames; frame++) {
                    float sum = 0;
                    for (uint32_t channel = 0; channel < channels; channel++) {
                        uint32_t bufferIndex = planar ? channel : 0;
                        if (bufferIndex >= list->mNumberBuffers) continue;
                        const AudioBuffer *buffer = &list->mBuffers[bufferIndex];
                        size_t index = planar ? frame : frame * channels + channel;
                        size_t bytes = isFloat ? sizeof(float) : sizeof(int16_t);
                        if (!buffer->mData || (index + 1) * bytes > buffer->mDataByteSize) continue;
                        sum += isFloat ? ((const float *)buffer->mData)[index] : (float)((const int16_t *)buffer->mData)[index] / 32768.0f;
                    }
                    mono[frame] = sum / channels;
                }
                self.receive(mono, frames, (uint32_t)format->mSampleRate, self.context);
            }
            free(mono);
        }
    }
    if (block) CFRelease(block);
    free(list);
}
@end

static void copy_error(char *error, size_t capacity, NSString *message) {
    if (!error || capacity == 0) return;
    const char *text = message.UTF8String ?: "Computer audio capture failed";
    snprintf(error, capacity, "%s", text);
}

// Called on the Rust meeting capture worker. No video output is registered or stored.
int pronto_capture_screen_audio(ProntoShouldStop shouldStop, ProntoAudioSamples receive,
                                void *context, char *error, size_t errorCapacity) {
    @autoreleasepool {
        if (@available(macOS 13.0, *)) {
            dispatch_semaphore_t ready = dispatch_semaphore_create(0);
            __block SCShareableContent *content = nil;
            __block NSError *failure = nil;
            [SCShareableContent getShareableContentExcludingDesktopWindows:YES onScreenWindowsOnly:NO
                completionHandler:^(SCShareableContent *found, NSError *problem) {
                    content = found;
                    failure = problem;
                    dispatch_semaphore_signal(ready);
                }];
            if (dispatch_semaphore_wait(ready, dispatch_time(DISPATCH_TIME_NOW, 15 * NSEC_PER_SEC)) != 0) {
                copy_error(error, errorCapacity, @"Timed out checking Screen Recording permission"); return -1;
            }
            if (failure || content.displays.count == 0) {
                copy_error(error, errorCapacity, failure.localizedDescription ?: @"No display available for computer audio capture"); return -1;
            }
            SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:content.displays.firstObject excludingApplications:@[] exceptingWindows:@[]];
            SCStreamConfiguration *configuration = [SCStreamConfiguration new];
            configuration.capturesAudio = YES;
            configuration.excludesCurrentProcessAudio = YES;
            configuration.sampleRate = 48000;
            configuration.channelCount = 1;
            configuration.width = 2;
            configuration.height = 2;
            ProntoAudioOutput *output = [ProntoAudioOutput new];
            output.receive = receive;
            output.context = context;
            SCStream *stream = [[SCStream alloc] initWithFilter:filter configuration:configuration delegate:output];
            NSError *addError = nil;
            if (![stream addStreamOutput:output type:SCStreamOutputTypeAudio sampleHandlerQueue:dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0) error:&addError]) {
                copy_error(error, errorCapacity, addError.localizedDescription); return -1;
            }
            dispatch_semaphore_t started = dispatch_semaphore_create(0);
            [stream startCaptureWithCompletionHandler:^(NSError *problem) {
                failure = problem;
                dispatch_semaphore_signal(started);
            }];
            if (dispatch_semaphore_wait(started, dispatch_time(DISPATCH_TIME_NOW, 15 * NSEC_PER_SEC)) != 0 || failure) {
                copy_error(error, errorCapacity, failure.localizedDescription ?: @"Timed out starting computer audio capture"); return -1;
            }
            while (!shouldStop(context)) {
                @synchronized(output) { if (output.stoppedError) break; }
                usleep(20000);
            }
            dispatch_semaphore_t stopped = dispatch_semaphore_create(0);
            [stream stopCaptureWithCompletionHandler:^(NSError *problem) {
                failure = problem;
                dispatch_semaphore_signal(stopped);
            }];
            dispatch_semaphore_wait(stopped, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC));
            @synchronized(output) {
                if (output.stoppedError) failure = output.stoppedError;
            }
            if (failure) { copy_error(error, errorCapacity, failure.localizedDescription); return -1; }
            return 0;
        }
        copy_error(error, errorCapacity, @"macOS 13 or newer is required for computer audio capture");
        return -1;
    }
}
