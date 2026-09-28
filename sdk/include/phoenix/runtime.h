#ifndef PHOENIX_RUNTIME_H
#define PHOENIX_RUNTIME_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define PHOENIX_FILE_OPEN_READ (UINT64_C(1) << 0)
#define PHOENIX_FILE_OPEN_WRITE (UINT64_C(1) << 1)
#define PHOENIX_FILE_OPEN_CREATE (UINT64_C(1) << 2)
#define PHOENIX_FILE_OPEN_TRUNCATE (UINT64_C(1) << 3)

#define PHOENIX_FILE_SEEK_START UINT64_C(0)
#define PHOENIX_FILE_SEEK_CURRENT UINT64_C(1)
#define PHOENIX_FILE_SEEK_END UINT64_C(2)

typedef struct PhoenixFileDescriptor {
    uint32_t slot;
    uint32_t generation;
} PhoenixFileDescriptor;

typedef struct PhoenixRuntimeCallResult {
    uint64_t value;
    uint64_t status;
} PhoenixRuntimeCallResult;

PhoenixRuntimeCallResult phoenix_file_open(
    const uint8_t *path,
    uint64_t path_length,
    uint64_t flags
);

PhoenixRuntimeCallResult phoenix_file_read(
    PhoenixFileDescriptor descriptor,
    uint8_t *buffer,
    uint64_t buffer_length
);

PhoenixRuntimeCallResult phoenix_file_write(
    PhoenixFileDescriptor descriptor,
    const uint8_t *buffer,
    uint64_t buffer_length
);

PhoenixRuntimeCallResult phoenix_file_seek(
    PhoenixFileDescriptor descriptor,
    int64_t offset,
    uint64_t origin
);

PhoenixRuntimeCallResult phoenix_file_close(PhoenixFileDescriptor descriptor);

#ifdef __cplusplus
}
#endif

#endif
