#import "xpc_server.h"

#import <Foundation/Foundation.h>
#import <Security/Security.h>
#import <libproc.h>
#import <xpc/xpc.h>

#include <errno.h>
#include <limits.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include <sys/proc.h>

static const char *const kJarvisPowerService =
    "app.jarvis.monitor.power-helper";
static const char *const kJarvisPowerPayload = "payload";

static bool jarvis_copy_string(
    CFTypeRef value,
    char *destination,
    size_t capacity
) {
    if (value == NULL || CFGetTypeID(value) != CFStringGetTypeID() ||
        destination == NULL || capacity < 2) {
        return false;
    }
    if (!CFStringGetCString(
            (CFStringRef)value,
            destination,
            (CFIndex)capacity,
            kCFStringEncodingUTF8)) {
        return false;
    }
    size_t length = strlen(destination);
    return length > 0 && length + 1 <= capacity;
}

static bool jarvis_parse_build(CFTypeRef value, uint64_t *build) {
    char text[32] = {0};
    if (build == NULL ||
        !jarvis_copy_string(value, text, sizeof(text))) {
        return false;
    }
    size_t length = strlen(text);
    if (length == 0) {
        return false;
    }
    for (size_t index = 0; index < length; ++index) {
        if (text[index] < '0' || text[index] > '9') {
            return false;
        }
    }
    errno = 0;
    char *end = NULL;
    unsigned long long parsed = strtoull(text, &end, 10);
    if (errno != 0 || end == text || *end != '\0' || parsed == 0) {
        return false;
    }
    *build = (uint64_t)parsed;
    return true;
}

static bool jarvis_copy_claims(
    xpc_object_t message,
    SecRequirementRef requirement,
    JarvisPowerClientClaims *claims
) {
    if (message == NULL || requirement == NULL || claims == NULL ||
        xpc_get_type(message) != XPC_TYPE_DICTIONARY) {
        return false;
    }
    memset(claims, 0, sizeof(*claims));

    SecCodeRef guest = NULL;
    OSStatus status =
        SecCodeCreateWithXPCMessage(message, kSecCSDefaultFlags, &guest);
    if (status != errSecSuccess || guest == NULL) {
        return false;
    }

    status = SecCodeCheckValidity(
        guest,
        kSecCSStrictValidate | kSecCSCheckAllArchitectures,
        requirement);
    if (status != errSecSuccess) {
        CFRelease(guest);
        return false;
    }

    CFDictionaryRef signing = NULL;
    status = SecCodeCopySigningInformation(
        guest, kSecCSSigningInformation, &signing);
    CFRelease(guest);
    if (status != errSecSuccess || signing == NULL) {
        return false;
    }

    CFTypeRef identifier =
        CFDictionaryGetValue(signing, kSecCodeInfoIdentifier);
    CFTypeRef team =
        CFDictionaryGetValue(signing, kSecCodeInfoTeamIdentifier);
    CFTypeRef plist_value =
        CFDictionaryGetValue(signing, kSecCodeInfoPList);
    bool signing_ok =
        jarvis_copy_string(
            team, claims->team_id, sizeof(claims->team_id)) &&
        jarvis_copy_string(
            identifier, claims->identifier, sizeof(claims->identifier)) &&
        plist_value != NULL &&
        CFGetTypeID(plist_value) == CFDictionaryGetTypeID();
    if (signing_ok) {
        CFTypeRef build = CFDictionaryGetValue(
            (CFDictionaryRef)plist_value, CFSTR("CFBundleVersion"));
        signing_ok = jarvis_parse_build(build, &claims->signed_build);
    }
    CFRelease(signing);
    if (!signing_ok) {
        memset(claims, 0, sizeof(*claims));
        return false;
    }

    xpc_connection_t peer = xpc_dictionary_get_remote_connection(message);
    if (peer == NULL) {
        memset(claims, 0, sizeof(*claims));
        return false;
    }
    pid_t pid = xpc_connection_get_pid(peer);
    uid_t euid = xpc_connection_get_euid(peer);
    if (pid <= 0 || euid == 0 || pid > INT32_MAX) {
        memset(claims, 0, sizeof(*claims));
        return false;
    }

    struct proc_bsdinfo process = {0};
    int copied = proc_pidinfo(
        pid, PROC_PIDTBSDINFO, 0, &process, (int)sizeof(process));
    if (copied != (int)sizeof(process) ||
        process.pbi_pid != (uint32_t)pid ||
        process.pbi_uid != euid ||
        process.pbi_status == SZOMB ||
        process.pbi_start_tvsec == 0 ||
        process.pbi_start_tvusec >= 1000000) {
        memset(claims, 0, sizeof(*claims));
        return false;
    }

    claims->euid = (uint32_t)euid;
    claims->pid = (int32_t)pid;
    claims->start_seconds = process.pbi_start_tvsec;
    claims->start_microseconds = (uint32_t)process.pbi_start_tvusec;
    return true;
}

static bool jarvis_extract_payload(
    xpc_object_t message,
    const uint8_t **bytes,
    size_t *length
) {
    if (message == NULL || bytes == NULL || length == NULL ||
        xpc_get_type(message) != XPC_TYPE_DICTIONARY) {
        return false;
    }
    __block size_t key_count = 0;
    __block bool exact_key = true;
    xpc_dictionary_apply(message, ^bool(const char *key, xpc_object_t value) {
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
        message, kJarvisPowerPayload, &payload_length);
    if (*bytes == NULL || payload_length == 0 ||
        payload_length > JARVIS_POWER_MAX_PAYLOAD) {
        return false;
    }
    *length = payload_length;
    return true;
}

int32_t jarvis_power_xpc_server_run(
    const char *service_label,
    const char *requirement_text,
    JarvisPowerMessageHandler handler,
    void *context
) {
    @autoreleasepool {
        if (service_label == NULL || requirement_text == NULL ||
            handler == NULL ||
            strcmp(service_label, kJarvisPowerService) != 0) {
            return 1;
        }

        CFStringRef requirement_string = CFStringCreateWithCString(
            kCFAllocatorDefault,
            requirement_text,
            kCFStringEncodingUTF8);
        if (requirement_string == NULL) {
            return 1;
        }
        SecRequirementRef requirement = NULL;
        OSStatus status = SecRequirementCreateWithString(
            requirement_string, kSecCSDefaultFlags, &requirement);
        CFRelease(requirement_string);
        if (status != errSecSuccess || requirement == NULL) {
            return 1;
        }

        dispatch_queue_t queue = dispatch_queue_create(
            "app.jarvis.monitor.power-helper.xpc", DISPATCH_QUEUE_SERIAL);
        xpc_connection_t listener = xpc_connection_create_mach_service(
            kJarvisPowerService,
            queue,
            XPC_CONNECTION_MACH_SERVICE_LISTENER);
        if (listener == NULL) {
            CFRelease(requirement);
            return 1;
        }

        xpc_connection_set_event_handler(
            listener, ^(xpc_object_t peer_object) {
                if (xpc_get_type(peer_object) != XPC_TYPE_CONNECTION) {
                    return;
                }
                xpc_connection_t peer = (xpc_connection_t)peer_object;
                xpc_connection_set_target_queue(peer, queue);
                xpc_connection_set_event_handler(
                    peer, ^(xpc_object_t message) {
                        if (xpc_get_type(message) == XPC_TYPE_ERROR) {
                            xpc_connection_cancel(peer);
                            return;
                        }

                        const uint8_t *payload = NULL;
                        size_t payload_length = 0;
                        JarvisPowerClientClaims first = {0};
                        JarvisPowerClientClaims second = {0};
                        if (!jarvis_copy_claims(
                                message, requirement, &first) ||
                            !jarvis_copy_claims(
                                message, requirement, &second) ||
                            memcmp(&first, &second, sizeof(first)) != 0 ||
                            !jarvis_extract_payload(
                                message, &payload, &payload_length)) {
                            xpc_connection_cancel(peer);
                            return;
                        }

                        uint8_t response[JARVIS_POWER_MAX_PAYLOAD] = {0};
                        size_t response_length = 0;
                        int32_t result = handler(
                            payload,
                            payload_length,
                            &first,
                            &second,
                            response,
                            sizeof(response),
                            &response_length,
                            context);
                        if (result != 0 || response_length == 0 ||
                            response_length > sizeof(response)) {
                            xpc_connection_cancel(peer);
                            return;
                        }

                        xpc_object_t reply =
                            xpc_dictionary_create_reply(message);
                        if (reply == NULL) {
                            xpc_connection_cancel(peer);
                            return;
                        }
                        xpc_dictionary_set_data(
                            reply,
                            kJarvisPowerPayload,
                            response,
                            response_length);
                        xpc_connection_send_message(peer, reply);
                    });
                xpc_connection_activate(peer);
            });
        xpc_connection_activate(listener);
        dispatch_main();
    }
}
