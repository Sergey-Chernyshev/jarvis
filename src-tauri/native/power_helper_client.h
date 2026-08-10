#ifndef JARVIS_POWER_HELPER_CLIENT_H
#define JARVIS_POWER_HELPER_CLIENT_H

#include <stddef.h>
#include <stdint.h>

typedef void (*JarvisPowerUnregisterCompletion)(int32_t status, void *context);

int32_t jarvis_power_helper_service_status(void);
int32_t jarvis_power_helper_service_register(void);
void jarvis_power_helper_service_unregister(
    JarvisPowerUnregisterCompletion completion,
    void *context
);
int32_t jarvis_power_helper_request(
    const uint8_t *request,
    size_t request_length,
    uint8_t *response,
    size_t response_capacity,
    size_t *response_length,
    uint32_t timeout_ms
);

#endif
