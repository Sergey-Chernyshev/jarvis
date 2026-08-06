#ifndef JARVIS_POWER_XPC_SERVER_H
#define JARVIS_POWER_XPC_SERVER_H

#include <stddef.h>
#include <stdint.h>

#define JARVIS_POWER_MAX_PAYLOAD ((size_t)16384)
#define JARVIS_POWER_TEAM_ID_CAPACITY ((size_t)11)
#define JARVIS_POWER_IDENTIFIER_CAPACITY ((size_t)129)

typedef struct {
    char team_id[JARVIS_POWER_TEAM_ID_CAPACITY];
    char identifier[JARVIS_POWER_IDENTIFIER_CAPACITY];
    uint64_t signed_build;
    uint32_t euid;
    int32_t pid;
    uint64_t start_seconds;
    uint32_t start_microseconds;
} JarvisPowerClientClaims;

typedef int32_t (*JarvisPowerMessageHandler)(
    const uint8_t *payload,
    size_t payload_length,
    const JarvisPowerClientClaims *first_claims,
    const JarvisPowerClientClaims *second_claims,
    uint8_t *response,
    size_t response_capacity,
    size_t *response_length,
    void *context
);

int32_t jarvis_power_xpc_server_run(
    const char *service_label,
    const char *requirement_text,
    JarvisPowerMessageHandler handler,
    void *context
);

#endif
