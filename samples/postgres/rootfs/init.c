#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <spawn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/mman.h>
#include <unistd.h>

extern char **environ;
static pid_t postmaster;
static int requested;
static int listener;

static void *control(void *unused)
{
    (void)unused;
    const char *identity = getenv("HL_PG_VM_ID");
    if (!identity || strlen(identity) != 32) {
        fprintf(stderr, "PG_INIT missing VM identity\n");
        exit(1);
    }
    while (!requested) {
        int fd = accept(listener, NULL, NULL);
        if (fd < 0 && errno == EINTR)
            continue;
        if (fd < 0) {
            fprintf(stderr, "PG_INIT accept: %s\n", strerror(errno));
            exit(1);
        }
        char command[34];
        size_t length = 0;
        while (length < sizeof(command) - 1) {
            ssize_t count = read(fd, command + length, 1);
            if (count < 0 && errno == EINTR)
                continue;
            if (count != 1)
                break;
            if (command[length++] == '\n')
                break;
        }
        command[length] = '\0';
        requested = length == 33 && command[32] == '\n' &&
                    memcmp(command, identity, 32) == 0;
        if (close(fd) != 0) {
            fprintf(stderr, "PG_INIT close: %s\n", strerror(errno));
            exit(1);
        }
        if (!requested)
            fprintf(stderr, "PG_INIT rejected invalid shutdown identity\n");
    }
    if (kill(postmaster, SIGINT) != 0) {
        fprintf(stderr, "PG_INIT signal: %s\n", strerror(errno));
        exit(1);
    }
    if (close(listener) != 0) {
        fprintf(stderr, "PG_INIT listener close: %s\n", strerror(errno));
        exit(1);
    }
    return NULL;
}

static void transfer(int fd, void *buffer, size_t size, int writing)
{
    char *bytes = buffer;
    while (size) {
        ssize_t count = writing ? write(fd, bytes, size) : read(fd, bytes, size);
        if (count < 0 && errno == EINTR)
            continue;
        if (count <= 0) {
            fprintf(stderr, "PG_INIT probe pipe: %s\n", strerror(errno));
            exit(1);
        }
        bytes += count;
        size -= (size_t)count;
    }
}

static void shared_creator_probe(void)
{
    int output[2];
    posix_spawn_file_actions_t actions;
    if (pipe(output) != 0 || posix_spawn_file_actions_init(&actions) != 0 ||
        posix_spawn_file_actions_adddup2(&actions, output[1], STDOUT_FILENO) != 0 ||
        posix_spawn_file_actions_addclose(&actions, output[0]) != 0 ||
        posix_spawn_file_actions_addclose(&actions, output[1]) != 0) {
        fprintf(stderr, "PG_INIT shared creator probe setup failed\n");
        exit(1);
    }
    char *args[] = {"/pg-init", "--shared-owner", NULL};
    pid_t creator;
    int error = posix_spawn(&creator, args[0], &actions, NULL, args, environ);
    if (error) {
        fprintf(stderr, "PG_INIT shared creator spawn: %s\n", strerror(error));
        exit(1);
    }
    if (posix_spawn_file_actions_destroy(&actions) != 0 || close(output[1]) != 0) {
        fprintf(stderr, "PG_INIT shared creator descriptor cleanup failed\n");
        exit(1);
    }
    uintptr_t address;
    transfer(output[0], &address, sizeof(address), 0);
    if (close(output[0]) != 0) {
        fprintf(stderr, "PG_INIT shared creator read descriptor close failed\n");
        exit(1);
    }
    int status;
    pid_t waited;
    do {
        waited = waitpid(creator, &status, 0);
    } while (waited < 0 && errno == EINTR);
    if (waited != creator || !WIFEXITED(status) || WEXITSTATUS(status) != 0 ||
        *(uint64_t *)address != UINT64_C(0x484c50474d454d31)) {
        fprintf(stderr, "PG_INIT shared mapping did not survive its creator\n");
        exit(1);
    }
    void *mapping = (void *)address;
    if (mremap(mapping, 8192, 4096, 0) != mapping ||
        mremap(mapping, 4096, 8192, 0) != mapping ||
        *(uint64_t *)mapping != UINT64_C(0x484c50474d454d31) ||
        munmap(mapping, 8192) != 0) {
        fprintf(stderr, "PG_INIT shared mapping remap/unmap failed: %s\n", strerror(errno));
        exit(1);
    }
    puts("PASS shared-creator-exit");
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--shared-owner") == 0) {
        void *mapping = mmap(NULL, 8192, PROT_READ | PROT_WRITE,
                             MAP_SHARED | MAP_ANONYMOUS, -1, 0);
        if (mapping == MAP_FAILED) {
            fprintf(stderr, "PG_INIT shared creator mmap: %s\n", strerror(errno));
            return 1;
        }
        *(uint64_t *)mapping = UINT64_C(0x484c50474d454d31);
        uintptr_t address = (uintptr_t)mapping;
        transfer(STDOUT_FILENO, &address, sizeof(address), 1);
        return 0;
    }
    shared_creator_probe();
    const char *data = getenv("HL_PG_DATA");
    if (data == NULL)
        data = "/pgdata";
    struct stat st;
    if (stat(data, &st) != 0 || st.st_uid != geteuid() || st.st_gid != getegid()) {
        fprintf(stderr, "PG_INIT data directory ownership does not match guest identity\n");
        return 1;
    }
    char temporary[] = "/tmp/pg-ownership-XXXXXX";
    int probe = mkstemp(temporary);
    if (probe < 0 || fstat(probe, &st) != 0 ||
        st.st_uid != geteuid() || st.st_gid != getegid() ||
        close(probe) != 0 || unlink(temporary) != 0) {
        fprintf(stderr, "PG_INIT newly created file ownership failed\n");
        return 1;
    }
    puts("PASS guest-ownership");
    listener = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in address = {
        .sin_family = AF_INET,
        .sin_addr.s_addr = htonl(INADDR_LOOPBACK),
        .sin_port = htons(55433)
    };
    int reuse = 1;
    if (listener < 0 || fcntl(listener, F_SETFD, FD_CLOEXEC) != 0 ||
        setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse)) != 0 ||
        bind(listener, (struct sockaddr *)&address, sizeof(address)) != 0 ||
        listen(listener, 1) != 0) {
        fprintf(stderr, "PG_INIT control listener: %s\n", strerror(errno));
        return 1;
    }
    char *args[] = {"/usr/local/bin/postgres", "-D", (char *)data, NULL};
    int error = posix_spawn(&postmaster, args[0], NULL, NULL, args, environ);
    if (error) {
        fprintf(stderr, "PG_INIT spawn: %s\n", strerror(error));
        return 1;
    }
    printf("PG_INIT postmaster=%ld uid=%ld\n", (long)postmaster, (long)geteuid());
    fflush(stdout);
    pthread_t reader;
    error = pthread_create(&reader, NULL, control, NULL);
    if (error) {
        fprintf(stderr, "PG_INIT control thread: %s\n", strerror(error));
        return 1;
    }
    int status;
    pid_t waited;
    do {
        waited = waitpid(postmaster, &status, 0);
    } while (waited < 0 && errno == EINTR);
    if (waited != postmaster || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "PG_INIT postmaster did not exit cleanly\n");
        return 1;
    }
    error = pthread_join(reader, NULL);
    if (error) {
        fprintf(stderr, "PG_INIT control join: %s\n", strerror(error));
        return 1;
    }
    if (!requested)
        return 1;
    puts("PASS postmaster-shutdown");
    return 0;
}
