#ifndef PHOENIX_ERRNO_H
#define PHOENIX_ERRNO_H

#define EIO 5
#define EBADF 9
#define EAGAIN 11
#define EINVAL 22
#define EMFILE 24
#define ENOSYS 38

int *__phoenix_errno_location(void);
#define errno (*__phoenix_errno_location())

#endif
