#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/file.h>
#include <unistd.h>

#define REQUIRE(c) do { if (!(c)) { fprintf(stderr, "FAIL storage line=%d %s errno=%d (%s)\n", \
    __LINE__, #c, errno, strerror(errno)); return 1; } } while (0)

int main(void)
{
    int fd = open("/storage/probes/io", O_CREAT | O_EXCL | O_RDWR, 0600);
    REQUIRE(fd >= 0);
    REQUIRE(write(fd, "abcdefgh", 8) == 8);
    REQUIRE(pwrite(fd, "XY", 2, 2) == 2);
    char buf[32] = {0};
    REQUIRE(pread(fd, buf, 8, 0) == 8 && memcmp(buf, "abXYefgh", 8) == 0);
    REQUIRE(ftruncate(fd, 4) == 0);
    REQUIRE(pread(fd, buf, 8, 0) == 4);
    REQUIRE(fsync(fd) == 0 && fdatasync(fd) == 0);
    struct stat st;
    REQUIRE(fstat(fd, &st) == 0 && st.st_size == 4 && (st.st_mode & 0777) == 0600 &&
            st.st_uid == geteuid() && st.st_gid == getegid());
    REQUIRE(fchmod(fd, 0640) == 0);
    REQUIRE(fstat(fd, &st) == 0 && (st.st_mode & 0777) == 0640);
    errno = 0;
    int collision = open("/storage/probes/io", O_CREAT | O_EXCL | O_RDWR, 0600);
    REQUIRE(collision < 0 && errno == EEXIST);
    struct flock lock = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 1};
    errno = 0;
    REQUIRE(fcntl(fd, F_SETLK, &lock) == -1 && errno == EOPNOTSUPP);
    errno = 0;
    REQUIRE(flock(fd, LOCK_EX | LOCK_NB) == -1 && errno == EOPNOTSUPP);
    REQUIRE(close(fd) == 0);
    fd = open("/storage/probes/io", O_WRONLY | O_APPEND);
    REQUIRE(fd >= 0);
    REQUIRE(lseek(fd, 0, SEEK_SET) == 0);
    REQUIRE(write(fd, "12", 2) == 2);
    REQUIRE(lseek(fd, 0, SEEK_CUR) == 6);
    REQUIRE(close(fd) == 0);
    REQUIRE(rename("/storage/probes/io", "/storage/probes/renamed") == 0);
    fd = open("/storage/probes/renamed", O_RDONLY);
    REQUIRE(fd >= 0 && read(fd, buf, sizeof(buf)) == 6 && memcmp(buf, "abXY12", 6) == 0);
    REQUIRE(close(fd) == 0);
    int directory = open("/storage/probes", O_RDONLY | O_DIRECTORY);
    REQUIRE(directory >= 0 && fsync(directory) == 0 && close(directory) == 0);
    REQUIRE(mkdir("/storage/probes/mode-dir", 0700) == 0);
    REQUIRE(stat("/storage/probes/mode-dir", &st) == 0 && (st.st_mode & 0777) == 0700);
    puts("PASS storage-offsets-modes-rename");

    fd = open("/storage/probes/sync-write", O_CREAT | O_WRONLY | O_SYNC, 0600);
    REQUIRE(fd >= 0 && write(fd, "sync", 4) == 4 && close(fd) == 0);
    fd = open("/storage/probes/dsync-write", O_CREAT | O_WRONLY | O_DSYNC, 0600);
    REQUIRE(fd >= 0 && write(fd, "data", 4) == 4 && close(fd) == 0);
    puts("PASS storage-synchronous-write");

    fd = open("/storage/probes/fail-sync", O_CREAT | O_RDWR, 0600);
    REQUIRE(fd >= 0 && write(fd, "failed sync", 11) == 11);
    errno = 0;
    REQUIRE(fsync(fd) == -1 && errno == EIO);
    errno = 0;
    REQUIRE(fdatasync(fd) == -1 && errno == EIO);
    REQUIRE(close(fd) == 0);
    directory = open("/storage/probes/fail-dir-sync", O_RDONLY | O_DIRECTORY);
    REQUIRE(directory >= 0);
    errno = 0;
    REQUIRE(fsync(directory) == -1 && errno == EIO);
    REQUIRE(close(directory) == 0);
    fd = open("/storage/probes/fail-sync", O_WRONLY | O_SYNC);
    REQUIRE(fd >= 0);
    errno = 0;
    REQUIRE(write(fd, "fail", 4) == -1 && errno == EIO);
    REQUIRE(close(fd) == 0);
    puts("PASS storage-sync-errors");

    fd = open("/storage/probes/partial-write", O_CREAT | O_RDWR, 0600);
    REQUIRE(fd >= 0);
    errno = 0;
    REQUIRE(write(fd, "1234567890123456", 16) == -1 && errno == EIO);
    REQUIRE(pread(fd, buf, 16, 0) == 7 && memcmp(buf, "1234567", 7) == 0);
    REQUIRE(close(fd) == 0);
    fd = open("/storage/probes/quota", O_CREAT | O_RDWR, 0600);
    REQUIRE(fd >= 0);
    errno = 0;
    REQUIRE(pwrite(fd, "x", 1, 512 * 1024 * 1024) == -1 && errno == EDQUOT);
    REQUIRE(close(fd) == 0);
    errno = 0;
    fd = open("/readonly/probes/denied", O_CREAT | O_RDWR, 0600);
    REQUIRE(fd == -1 && errno == EROFS);
    puts("PASS storage-write-errors");

    DIR *entries = opendir("/storage/probes/many");
    REQUIRE(entries != NULL);
    unsigned count = 0;
    struct dirent *entry;
    errno = 0;
    while ((entry = readdir(entries)) != NULL) {
        if (!strcmp(entry->d_name, ".") || !strcmp(entry->d_name, ".."))
            continue;
        char expected[32];
        snprintf(expected, sizeof(expected), "entry-%04u", count);
        REQUIRE(strcmp(entry->d_name, expected) == 0);
        count++;
    }
    REQUIRE(errno == 0 && count == 5000);
    rewinddir(entries);
    entry = readdir(entries);
    REQUIRE(entry != NULL && strcmp(entry->d_name, "entry-0000") == 0);
    REQUIRE(closedir(entries) == 0);
    puts("PASS storage-pagination");
    puts("PASS storage");
    return 0;
}
