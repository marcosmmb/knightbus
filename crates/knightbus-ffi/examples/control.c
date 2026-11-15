/* Check the C API: open a connection, request Info, then release its resources.
 * Service:  ./control ENDPOINT
 * Embedded: ./control --embedded CONFIG
 * Link with libknightbus; see docs/control-api.md for the message format.
 */
#include "knightbus.h"

#include <limits.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif

enum {
    MAX_CONFIG_BYTES = 16384,
    MILLISECONDS_PER_SECOND = 1000,
    SERVICE_ARGUMENT_COUNT = 2,
    EMBEDDED_ARGUMENT_COUNT = 3,
    REQUEST_TIMEOUT_MS = 5000,
    EMBEDDED_RUN_SECONDS = 5,
    INFO_COMMAND = 12,
    REQUEST_BODY_BYTES = sizeof(uint16_t) + sizeof(uint64_t) + sizeof(uint64_t) + sizeof(uint8_t),
    REQUEST_FRAME_BYTES = sizeof(uint32_t) + REQUEST_BODY_BYTES,
    RESPONSE_ERROR_OFFSET = sizeof(uint32_t) + sizeof(uint64_t),
    RESPONSE_HEADER_BYTES = RESPONSE_ERROR_OFFSET + sizeof(uint16_t)
};

static bool check_status(const char *operation, uint16_t status) {
    if (status == 0) {
        return true;
    }
    fprintf(stderr, "%s failed: error=%u\n", operation, (unsigned)status);
    return false;
}

static bool open_embedded(const char *config_path, KbHandle **handle) {
    FILE *file = fopen(config_path, "rb");
    if (file == NULL) {
        perror("open configuration");
        return false;
    }

    /* Read one extra byte to detect a configuration over the size limit. */
    uint8_t config[MAX_CONFIG_BYTES + 1];
    size_t length = fread(config, 1, sizeof config, file);
    bool read_failed = ferror(file) != 0;
    fclose(file);

    if (read_failed) {
        fprintf(stderr, "Could not read configuration\n");
        return false;
    }
    if (length > MAX_CONFIG_BYTES) {
        fprintf(stderr, "Configuration exceeds %u bytes\n", MAX_CONFIG_BYTES);
        return false;
    }

    /* The library copies its inputs; config can go out of scope after this call. */
    uint16_t status = kb_embedded_open(KB_ABI_VERSION, config, length, handle);
    return check_status("open embedded host", status);
}

/* Encode fields explicitly: C struct padding is not part of the wire format. */
static void write_little_endian(uint8_t **cursor, uint64_t value, size_t width) {
    for (size_t byte = 0; byte < width; ++byte) {
        *(*cursor)++ = (uint8_t)value;
        value >>= CHAR_BIT;
    }
}

static bool check_info_response(const KbBuffer *response) {
    const uint8_t *bytes = kb_buffer_data(response);
    size_t length = kb_buffer_len(response);
    if (length < RESPONSE_HEADER_BYTES) {
        fprintf(stderr, "Info response is missing its header\n");
        return false;
    }

    /* The operation's result is separate from kb_call's own return status. */
    const uint8_t *error = bytes + RESPONSE_ERROR_OFFSET;
    uint16_t status = (uint16_t)(error[0] | ((uint16_t)error[1] << CHAR_BIT));
    printf("response_bytes=%zu operation_error=%u\n", length, (unsigned)status);
    return check_status("Info request", status);
}

static bool request_info(KbHandle *handle) {
    uint8_t request[REQUEST_FRAME_BYTES];
    uint8_t *cursor = request;
    write_little_endian(&cursor, REQUEST_BODY_BYTES, sizeof(uint32_t));
    write_little_endian(&cursor, KB_CONTROL_VERSION, sizeof(uint16_t));
    write_little_endian(&cursor, 1, sizeof(uint64_t)); /* First request ID. */
    write_little_endian(&cursor, 0, sizeof(uint64_t)); /* kb_call supplies the deadline. */
    write_little_endian(&cursor, INFO_COMMAND, sizeof(uint8_t));

    KbBuffer *response = NULL;
    uint16_t status = kb_call(handle, request, sizeof request, REQUEST_TIMEOUT_MS, &response);
    if (!check_status("control call", status)) {
        return false;
    }

    bool success = check_info_response(response);
    kb_buffer_free(response); /* Release with the library's allocator, not free(). */
    return success;
}

static void run_embedded_briefly(void) {
    /* Keep the embedded host alive for five seconds before requesting shutdown. */
#ifdef _WIN32
    Sleep(EMBEDDED_RUN_SECONDS * MILLISECONDS_PER_SECOND);
#else
    sleep(EMBEDDED_RUN_SECONDS);
#endif
}

int main(int argc, char **argv) {
    bool embedded = argc == EMBEDDED_ARGUMENT_COUNT && strcmp(argv[1], "--embedded") == 0;
    bool service = argc == SERVICE_ARGUMENT_COUNT && strcmp(argv[1], "--embedded") != 0;
    if (!embedded && !service) {
        fprintf(stderr, "usage: control ENDPOINT | --embedded CONFIG\n");
        return EXIT_FAILURE;
    }

    KbHandle *handle = NULL;
    if (embedded) {
        if (!open_embedded(argv[argc - 1], &handle)) {
            return EXIT_FAILURE;
        }
    } else {
        const char *endpoint = argv[1];
        uint16_t status = kb_service_open(
            KB_ABI_VERSION, (const uint8_t *)endpoint, strlen(endpoint), &handle);
        if (!check_status("connect to service", status)) {
            return EXIT_FAILURE;
        }
    }

    bool success = request_info(handle);
    if (embedded && success) {
        run_embedded_briefly();
    }

    /* Embedded close stops its host; Service close leaves the daemon running. */
    bool closed = check_status("close handle", kb_close(handle));
    return success && closed ? EXIT_SUCCESS : EXIT_FAILURE;
}
