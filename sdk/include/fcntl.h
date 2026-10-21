#ifndef PHOENIX_FCNTL_H
#define PHOENIX_FCNTL_H

#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR 2
#define O_CREAT 0x40
#define O_TRUNC 0x200

int phoenix_libc_open(const char *path, int flags);
#define open(path, flags, ...) phoenix_libc_open((path), (flags))

#endif
