#ifndef KNIGHTBUS_H
#define KNIGHTBUS_H
#include <stddef.h>
#include <stdint.h>
#ifdef _WIN32
# ifdef KNIGHTBUS_STATIC
#  define KB_API
# else
#  define KB_API __declspec(dllimport)
# endif
#else
# define KB_API
#endif
#ifdef __cplusplus
extern "C" {
#endif
/* One thread at a time per handle. Different handles may run concurrently.
 * All pointers must be valid for their documented sizes. NULL output pointers
 * are rejected. No call retains an input pointer. Never copy/free opaque objects
 * with the host allocator. UTF-8 inputs exclude a trailing NUL.
 * Embedded mode requires the same native permissions as the daemon.
 */
typedef struct KbHandle KbHandle;
typedef struct KbBuffer KbBuffer;
#define KB_ABI_VERSION 1u
#define KB_CONTROL_VERSION 1u
#define KB_MAX_FRAME 8192u
/* Return codes: 0 success, 1 invalid, 2 state, 3 unsupported, 4 unavailable,
 * 5 denied, 6 not found, 7 version, 8 deadline, 9 cancelled, 10 exhausted,
 * 11 internal/panic, 12 cleanup (native resources still quarantined).
 */
KB_API uint32_t kb_abi_version(void);
KB_API uint16_t kb_control_version(void);
KB_API uint16_t kb_embedded_open(uint32_t abi, const uint8_t *config, size_t len, KbHandle **out);
KB_API uint16_t kb_service_open(uint32_t abi, const uint8_t *path, size_t len, KbHandle **out);
/* Frames use the versioned management schema described in docs/control-api.md.
 * IDs must increase. timeout_ms is relative; the request's deadline field is
 * ignored by this function. An accepted attach returns a session immediately;
 * cancel it with Detach, then observe Status/events until native retirement.
 * Subscribe/Poll/Unsubscribe implement bounded asynchronous event delivery.
 * No callbacks execute under engine locks. A response owns its error code;
 * no thread-local or borrowed error string exists.
 */
KB_API uint16_t kb_call(KbHandle *, const uint8_t *frame, size_t len, uint64_t timeout_ms, KbBuffer **out);
KB_API const uint8_t *kb_buffer_data(const KbBuffer *);
KB_API size_t kb_buffer_len(const KbBuffer *);
KB_API void kb_buffer_free(KbBuffer *);
KB_API uint16_t kb_close(KbHandle *);
#ifdef __cplusplus
}
#endif
#endif
