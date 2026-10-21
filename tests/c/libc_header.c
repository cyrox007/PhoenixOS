#include <errno.h>
#include <fcntl.h>
#include <unistd.h>

_Static_assert(O_RDONLY == 0, "O_RDONLY изменился");
_Static_assert(O_WRONLY == 1, "O_WRONLY изменился");
_Static_assert(O_RDWR == 2, "O_RDWR изменился");
_Static_assert(sizeof(off_t) == 8, "off_t должен быть 64-битным");

int check_api(const char *path, void *buffer, size_t size) {
    int fd = open(path, O_RDWR | O_CREAT);
    if (fd < 0) {
        return errno;
    }

    if (write(fd, buffer, size) < 0) {
        return errno;
    }
    if (lseek(fd, 0, SEEK_SET) < 0) {
        return errno;
    }
    if (read(fd, buffer, size) < 0) {
        return errno;
    }
    return close(fd);
}
