/*
 * offline_first_core: the C ABI v2 (RFC-001 §12.2).
 *
 * Handles are 64-bit values validated by the library, never pointers: a
 * closed, released or made-up handle answers LDB_INVALID_HANDLE. Requests
 * and responses are bytes with a length; responses are wire-protocol JSON
 * (https://github.com/JhonaCodes/db_dsl/blob/main/PROTOCOL.md) in buffers
 * that the library owns until ldb_buffer_release.
 *
 * The ofc_* symbols of ABI v1 remain available.
 */
#ifndef LOCALDB_H
#define LOCALDB_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef uint64_t LdbHandle;
typedef uint64_t LdbBufferHandle;

#define LDB_ABI_VERSION 2

#define LDB_OK 0
#define LDB_INVALID_HANDLE 1
#define LDB_NULL_POINTER 2
#define LDB_INVALID_UTF8 3
#define LDB_PANIC 4
#define LDB_REQUEST_TOO_LARGE 5

#define LDB_MAX_REQUEST_BYTES (256u << 20)

/* The version of the ABI (2). */
uint32_t ldb_abi_version(void);

/* Opens (or shares) the database at <path>.lmdb. `options` is JSON, or empty
 * for the defaults. Writes the handle to `out` (0 on failure) and the wire
 * response ({"v":1,"ok":{}} or an error) to `response`. */
int32_t ldb_open(const uint8_t *path, size_t path_len,
                 const uint8_t *options, size_t options_len,
                 LdbHandle *out, LdbBufferHandle *response);

/* Runs one wire-protocol request; writes the handle of its response. */
int32_t ldb_execute(LdbHandle database, const uint8_t *request,
                    size_t request_len, LdbBufferHandle *response);

/* Where the bytes of `buffer` are and how many, until it is released. */
int32_t ldb_buffer_view(LdbBufferHandle buffer, const uint8_t **data,
                        size_t *len);

/* Releases a buffer; a second release answers LDB_INVALID_HANDLE. */
int32_t ldb_buffer_release(LdbBufferHandle buffer);

/* Closes a database handle; a second close answers LDB_INVALID_HANDLE. */
int32_t ldb_close(LdbHandle database);

#ifdef __cplusplus
}
#endif

#endif /* LOCALDB_H */
