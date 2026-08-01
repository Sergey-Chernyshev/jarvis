#import "power_helper_client.h"

#import <Foundation/Foundation.h>
#import <ServiceManagement/ServiceManagement.h>
#import <xpc/xpc.h>

#include <stdbool.h>
#include <string.h>

static const char *const kJarvisPowerService =
    "app.jarvis.monitor.power-helper";
static NSString *const kJarvisPowerPlist =
    @"app.jarvis.monitor.power-helper.plist";
static const char *const kJarvisPowerPayload = "payload";
static const size_t kJarvisPowerMaxPayload = 16384;

static SMAppService *jarvis_power_service(void) {
    return [SMAppService daemonServiceWithPlistName:kJarvisPowerPlist];
}

static int32_t jarvis_power_status(SMAppServiceStatus status) {
    switch (status) {
        case SMAppServiceStatusNotRegistered:
            return 0;
        case SMAppServiceStatusEnabled:
            return 1;
        case SMAppServiceStatusRequiresApproval:
            return 2;
        case SMAppServiceStatusNotFound:
            return 3;
    }
    return -1;
}

int32_t jarvis_power_helper_service_status(void) {
    @autoreleasepool {
        return jarvis_power_status(jarvis_power_service().status);
    }
}

int32_t jarvis_power_helper_service_register(void) {
    @autoreleasepool {
        NSError *error = nil;
        if (![jarvis_power_service() registerAndReturnError:&error] ||
            error != nil) {
            return 1;
        }
        return 0;
    }
}

void jarvis_power_helper_service_unregister(
    JarvisPowerUnregisterCompletion completion,
    void *context
) {
    if (completion == NULL) {
        return;
    }
    @autoreleasepool {
        [jarvis_power_service()
            unregisterWithCompletionHandler:^(NSError *error) {
                completion(error == nil ? 0 : 1, context);
            }];
    }
}

static bool jarvis_power_exact_reply(
    xpc_object_t reply,
    const uint8_t **bytes,
    size_t *length
) {
    if (reply == NULL || bytes == NULL || length == NULL ||
        xpc_get_type(reply) != XPC_TYPE_DICTIONARY) {
        return false;
    }
    __block size_t key_count = 0;
    __block bool exact_key = true;
    xpc_dictionary_apply(reply, ^bool(const char *key, xpc_object_t value) {
        ++key_count;
        if (strcmp(key, kJarvisPowerPayload) != 0 ||
            xpc_get_type(value) != XPC_TYPE_DATA) {
            exact_key = false;
        }
        return true;
    });
    if (!exact_key || key_count != 1) {
        return false;
    }
    size_t payload_length = 0;
    *bytes = xpc_dictionary_get_data(
        reply, kJarvisPowerPayload, &payload_length);
    if (*bytes == NULL || payload_length == 0 ||
        payload_length > kJarvisPowerMaxPayload) {
        return false;
    }
    *length = payload_length;
    return true;
}

int32_t jarvis_power_helper_request(
    const uint8_t *request,
    size_t request_length,
    uint8_t *response,
    size_t response_capacity,
    size_t *response_length,
    uint32_t timeout_ms
) {
    @autoreleasepool {
        if (request == NULL || request_length == 0 ||
            request_length > kJarvisPowerMaxPayload ||
            response == NULL || response_capacity == 0 ||
            response_capacity > kJarvisPowerMaxPayload ||
            response_length == NULL || timeout_ms == 0) {
            return 3;
        }
        *response_length = 0;

        dispatch_queue_t queue = dispatch_queue_create(
            "app.jarvis.monitor.power-helper.client",
            DISPATCH_QUEUE_SERIAL);
        xpc_connection_t connection = xpc_connection_create_mach_service(
            kJarvisPowerService, queue, 0);
        if (connection == NULL) {
            return 1;
        }
        xpc_connection_set_event_handler(
            connection, ^(xpc_object_t event) {
                (void)event;
            });
        xpc_connection_activate(connection);

        xpc_object_t message = xpc_dictionary_create(NULL, NULL, 0);
        xpc_dictionary_set_data(
            message, kJarvisPowerPayload, request, request_length);

        dispatch_semaphore_t completion = dispatch_semaphore_create(0);
        __block NSData *reply_data = nil;
        __block int32_t reply_status = 1;
        xpc_connection_send_message_with_reply(
            connection, message, queue, ^(xpc_object_t reply) {
                const uint8_t *bytes = NULL;
                size_t length = 0;
                if (jarvis_power_exact_reply(reply, &bytes, &length)) {
                    reply_data = [NSData dataWithBytes:bytes length:length];
                    reply_status = 0;
                }
                dispatch_semaphore_signal(completion);
            });

        dispatch_time_t deadline = dispatch_time(
            DISPATCH_TIME_NOW, (int64_t)timeout_ms * NSEC_PER_MSEC);
        if (dispatch_semaphore_wait(completion, deadline) != 0) {
            xpc_connection_cancel(connection);
            return 2;
        }
        xpc_connection_cancel(connection);
        if (reply_status != 0 || reply_data == nil ||
            reply_data.length == 0 ||
            reply_data.length > response_capacity) {
            return 3;
        }
        memcpy(response, reply_data.bytes, reply_data.length);
        *response_length = reply_data.length;
        return 0;
    }
}
