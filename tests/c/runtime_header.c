#include <phoenix/runtime.h>

_Static_assert(sizeof(PhoenixFileDescriptor) == 8, "размер дескриптора изменился");
_Static_assert(sizeof(PhoenixRuntimeCallResult) == 16, "размер результата изменился");

int main(void) {
    PhoenixFileDescriptor descriptor = { .slot = 1, .generation = 2 };
    PhoenixRuntimeCallResult result = { .value = descriptor.slot, .status = 0 };

    if (PHOENIX_FILE_OPEN_READ != UINT64_C(1)) {
        return 1;
    }
    if (PHOENIX_FILE_SEEK_END != UINT64_C(2)) {
        return 2;
    }
    return result.status != 0;
}
