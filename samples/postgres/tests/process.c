#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <semaphore.h>
#include <signal.h>
#include <spawn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/signalfd.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;
static const char *executable;
static _Thread_local unsigned tls_value = 17;
static unsigned global_value = 19;

#define REQUIRE(condition) do { \
    if (!(condition)) { \
        fprintf(stderr, "FAIL line=%d %s errno=%d (%s)\n", __LINE__, \
                #condition, errno, strerror(errno)); \
        exit(1); \
    } \
} while (0)

static void transfer(int fd, void *buffer, size_t length, int writing)
{
    unsigned char *bytes = buffer;
    while (length) {
        ssize_t count = writing ? write(fd, bytes, length) : read(fd, bytes, length);
        if (count < 0 && errno == EINTR)
            continue;
        REQUIRE(count > 0);
        bytes += count;
        length -= (size_t)count;
    }
}

static pid_t spawn(char *const args[], posix_spawn_file_actions_t *actions)
{
    pid_t pid;
    int error = posix_spawn(&pid, executable, actions, NULL, args, environ);
    if (error) {
        errno = error;
        REQUIRE(error == 0);
    }
    REQUIRE(pid > 0);
    return pid;
}

static void wait_exit(pid_t pid, int expected)
{
    int status;
    pid_t waited;
    do {
        waited = waitpid(pid, &status, 0);
    } while (waited < 0 && errno == EINTR);
    REQUIRE(waited == pid);
    REQUIRE(WIFEXITED(status));
    REQUIRE(WEXITSTATUS(status) == expected);
}

static void wire_pipe(posix_spawn_file_actions_t *actions, int input[2], int output[2])
{
    REQUIRE(pipe(input) == 0);
    REQUIRE(pipe(output) == 0);
    REQUIRE(posix_spawn_file_actions_init(actions) == 0);
    REQUIRE(posix_spawn_file_actions_adddup2(actions, input[0], STDIN_FILENO) == 0);
    REQUIRE(posix_spawn_file_actions_adddup2(actions, output[1], STDOUT_FILENO) == 0);
    for (int i = 0; i < 2; i++) {
        REQUIRE(posix_spawn_file_actions_addclose(actions, input[i]) == 0);
        REQUIRE(posix_spawn_file_actions_addclose(actions, output[i]) == 0);
    }
}

static void close_child_ends(posix_spawn_file_actions_t *actions, int input[2], int output[2])
{
    REQUIRE(posix_spawn_file_actions_destroy(actions) == 0);
    REQUIRE(close(input[0]) == 0);
    REQUIRE(close(output[1]) == 0);
}

static int signal_fd;
static void signal_handler(int signal_number)
{
    unsigned char byte = (unsigned char)signal_number;
    int saved = errno;
    if (write(signal_fd, &byte, 1) != 1)
        _exit(90);
    errno = saved;
}

static void *thread_tls(void *argument)
{
    unsigned value = *(unsigned *)argument;
    REQUIRE(tls_value == 17);
    tls_value = value;
    REQUIRE(sched_yield() == 0);
    REQUIRE(tls_value == value);
    return NULL;
}

static void *thread_signal(void *argument)
{
    int *fds = argument;
    signal_fd = fds[1];
    struct sigaction action = {.sa_handler = signal_handler};
    REQUIRE(sigemptyset(&action.sa_mask) == 0);
    REQUIRE(sigaction(SIGUSR1, &action, NULL) == 0);
    unsigned char byte = 42;
    transfer(fds[1], &byte, 1, 1);
    transfer(fds[0], &byte, 1, 0);
    REQUIRE(byte == 99);
    return NULL;
}

__attribute__((noinline)) static unsigned touch_stack(unsigned depth)
{
    volatile unsigned char block[32768];
    for (size_t i = 0; i < sizeof(block); i += 4096)
        block[i] = (unsigned char)depth;
    unsigned result = depth ? touch_stack(depth - 1) : 0;
    for (size_t i = 0; i < sizeof(block); i += 4096)
        result += block[i];
    return result;
}

static void check_stack(void)
{
    struct rlimit limit;
    REQUIRE(getrlimit(RLIMIT_STACK, &limit) == 0);
    REQUIRE(limit.rlim_cur == RLIM_INFINITY || limit.rlim_cur >= 1024 * 1024);
    REQUIRE(touch_stack(31) == 8 * (31 * 32 / 2));
    pthread_attr_t attr;
    void *base;
    size_t size;
    REQUIRE(pthread_getattr_np(pthread_self(), &attr) == 0);
    REQUIRE(pthread_attr_getstack(&attr, &base, &size) == 0);
    REQUIRE(pthread_attr_destroy(&attr) == 0);
    printf("STACK pid=%ld limit=%llu reported=%zu touched=1048576\n",
           (long)getpid(), (unsigned long long)limit.rlim_cur, size);
    REQUIRE(size >= 1024 * 1024);
}

struct shared_state {
    sem_t ready;
    sem_t reply;
    unsigned value;
};

static void sem_wait_checked(sem_t *semaphore)
{
    int result;
    do {
        result = sem_wait(semaphore);
    } while (result && errno == EINTR);
    REQUIRE(result == 0);
}

static int child(int argc, char **argv)
{
    REQUIRE(argc >= 3);
    REQUIRE(tls_value == 17 && global_value == 19);
    tls_value = 123;
    global_value = 125;
    if (!strcmp(argv[2], "exit"))
        return 23;
    if (!strcmp(argv[2], "pipe")) {
        unsigned char data[128];
        transfer(STDIN_FILENO, data, sizeof(data), 0);
        for (size_t i = 0; i < sizeof(data); i++)
            data[i] ^= 0x5a;
        transfer(STDOUT_FILENO, data, sizeof(data), 1);
        return 0;
    }
    if (!strcmp(argv[2], "signal")) {
        signal_fd = STDOUT_FILENO;
        struct sigaction action = {.sa_handler = signal_handler};
        REQUIRE(sigemptyset(&action.sa_mask) == 0);
        REQUIRE(sigaction(SIGUSR1, &action, NULL) == 0);
        unsigned char byte = 42;
        transfer(STDOUT_FILENO, &byte, 1, 1);
        transfer(STDIN_FILENO, &byte, 1, 0);
        REQUIRE(byte == 99);
        return 0;
    }
    if (!strcmp(argv[2], "blocked")) {
        sigset_t mask;
        REQUIRE(sigprocmask(SIG_SETMASK, NULL, &mask) == 0);
        REQUIRE(sigismember(&mask, SIGUSR1) == 1);
        int fd = signalfd(-1, &mask, SFD_CLOEXEC);
        REQUIRE(fd >= 0);
        unsigned char byte = 42;
        transfer(STDOUT_FILENO, &byte, 1, 1);
        struct signalfd_siginfo info;
        transfer(fd, &info, sizeof(info), 0);
        REQUIRE(info.ssi_signo == SIGUSR1);
        REQUIRE(close(fd) == 0);
        return 0;
    }
    if (!strcmp(argv[2], "setsid")) {
        REQUIRE(setsid() == getpid());
        REQUIRE(getsid(0) == getpid());
        REQUIRE(getpgrp() == getpid());
        return 0;
    }
    if (!strcmp(argv[2], "stack")) {
        check_stack();
        return 0;
    }
    if (!strcmp(argv[2], "shared")) {
        REQUIRE(argc == 5);
        struct shared_state *state;
        if (!strcmp(argv[3], "address")) {
            char *end;
            uintptr_t address = (uintptr_t)strtoull(argv[4], &end, 16);
            REQUIRE(address && !*end);
            state = (struct shared_state *)address;
        } else {
            REQUIRE(!strcmp(argv[3], "file"));
            int fd = open(argv[4], O_RDWR);
            REQUIRE(fd >= 0);
            state = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
            REQUIRE(state != MAP_FAILED);
            REQUIRE(close(fd) == 0);
        }
        for (unsigned i = 0; i < 32; i++) {
            sem_wait_checked(&state->ready);
            REQUIRE(state->value == i);
            state->value = i + 100;
            REQUIRE(sem_post(&state->reply) == 0);
        }
        return 0;
    }
    REQUIRE(!"unknown child mode");
    return 1;
}

static void probe_spawn(void)
{
    tls_value = 77;
    global_value = 79;
    for (int i = 0; i < 32; i++) {
        char *args[] = {(char *)executable, "--child", "exit", NULL};
        wait_exit(spawn(args, NULL), 23);
        REQUIRE(tls_value == 77 && global_value == 79);
    }
    int status;
    errno = 0;
    REQUIRE(waitpid(-1, &status, WNOHANG) == -1 && errno == ECHILD);
}

static void probe_pipes(void)
{
    for (int iteration = 0; iteration < 8; iteration++) {
        int input[2], output[2];
        posix_spawn_file_actions_t actions;
        wire_pipe(&actions, input, output);
        char *args[] = {(char *)executable, "--child", "pipe", NULL};
        pid_t pid = spawn(args, &actions);
        close_child_ends(&actions, input, output);
        unsigned char data[128];
        for (size_t i = 0; i < sizeof(data); i++)
            data[i] = (unsigned char)(i + iteration);
        transfer(input[1], data, sizeof(data), 1);
        REQUIRE(close(input[1]) == 0);
        transfer(output[0], data, sizeof(data), 0);
        for (size_t i = 0; i < sizeof(data); i++)
            REQUIRE(data[i] == ((unsigned char)(i + iteration) ^ 0x5a));
        REQUIRE(read(output[0], data, 1) == 0);
        REQUIRE(close(output[0]) == 0);
        wait_exit(pid, 0);
    }
}

static void probe_signals(int blocked)
{
    sigset_t mask, previous;
    REQUIRE(sigemptyset(&mask) == 0);
    REQUIRE(sigaddset(&mask, SIGUSR1) == 0);
    if (blocked)
        REQUIRE(sigprocmask(SIG_BLOCK, &mask, &previous) == 0);
    int input[2], output[2];
    posix_spawn_file_actions_t actions;
    wire_pipe(&actions, input, output);
    char *args[] = {(char *)executable, "--child", blocked ? "blocked" : "signal", NULL};
    pid_t pid = spawn(args, &actions);
    close_child_ends(&actions, input, output);
    unsigned char byte;
    transfer(output[0], &byte, 1, 0);
    REQUIRE(byte == 42);
    REQUIRE(kill(pid, SIGUSR1) == 0);
    if (!blocked) {
        transfer(output[0], &byte, 1, 0);
        REQUIRE(byte == SIGUSR1);
        byte = 99;
        transfer(input[1], &byte, 1, 1);
    }
    wait_exit(pid, 0);
    REQUIRE(close(input[1]) == 0);
    REQUIRE(close(output[0]) == 0);
    if (blocked) {
        int fd = signalfd(-1, &mask, SFD_CLOEXEC | SFD_NONBLOCK);
        REQUIRE(fd >= 0);
        struct signalfd_siginfo info;
        errno = 0;
        REQUIRE(read(fd, &info, sizeof(info)) == -1 && errno == EAGAIN);
        REQUIRE(kill(getpid(), SIGUSR1) == 0);
        transfer(fd, &info, sizeof(info), 0);
        REQUIRE(info.ssi_signo == SIGUSR1);
        REQUIRE(close(fd) == 0);
        REQUIRE(sigprocmask(SIG_SETMASK, &previous, NULL) == 0);
    } else {
        wire_pipe(&actions, input, output);
        pid = spawn(args, &actions);
        close_child_ends(&actions, input, output);
        transfer(output[0], &byte, 1, 0);
        REQUIRE(byte == 42);
        REQUIRE(kill(pid, SIGTERM) == 0);
        int status;
        REQUIRE(waitpid(pid, &status, 0) == pid);
        REQUIRE(WIFSIGNALED(status) && WTERMSIG(status) == SIGTERM);
        REQUIRE(close(input[1]) == 0);
        REQUIRE(close(output[0]) == 0);
        REQUIRE(pipe(input) == 0);
        REQUIRE(pipe(output) == 0);
        int fds[] = {input[0], output[1]};
        pthread_t thread;
        REQUIRE(pthread_create(&thread, NULL, thread_signal, fds) == 0);
        transfer(output[0], &byte, 1, 0);
        REQUIRE(byte == 42);
        REQUIRE(pthread_kill(thread, SIGUSR1) == 0);
        transfer(output[0], &byte, 1, 0);
        REQUIRE(byte == SIGUSR1);
        byte = 99;
        transfer(input[1], &byte, 1, 1);
        REQUIRE(pthread_join(thread, NULL) == 0);
        for (int i = 0; i < 2; i++) {
            REQUIRE(close(input[i]) == 0);
            REQUIRE(close(output[i]) == 0);
        }
    }
}

static void probe_shared(void)
{
    int guest = getenv("PHASE1_SHARED_VM") && !strcmp(getenv("PHASE1_SHARED_VM"), "1");
    char filename[] = "/tmp/phase1-shared-XXXXXX";
    int fd = -1;
    if (!guest) {
        fd = mkstemp(filename);
        REQUIRE(fd >= 0);
        REQUIRE(ftruncate(fd, 4096) == 0);
    }
    struct shared_state *state = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                                     MAP_SHARED | (guest ? MAP_ANONYMOUS : 0), fd, 0);
    REQUIRE(state != MAP_FAILED);
    if (fd >= 0)
        REQUIRE(close(fd) == 0);
    REQUIRE(sem_init(&state->ready, 1, 0) == 0);
    REQUIRE(sem_init(&state->reply, 1, 0) == 0);
    char address[32];
    snprintf(address, sizeof(address), "%llx", (unsigned long long)(uintptr_t)state);
    char *args[] = {(char *)executable, "--child", "shared",
                    guest ? "address" : "file", guest ? address : filename, NULL};
    pid_t pid = spawn(args, NULL);
    for (unsigned i = 0; i < 32; i++) {
        state->value = i;
        REQUIRE(sem_post(&state->ready) == 0);
        sem_wait_checked(&state->reply);
        REQUIRE(state->value == i + 100);
    }
    wait_exit(pid, 0);
    REQUIRE(sem_destroy(&state->ready) == 0);
    REQUIRE(sem_destroy(&state->reply) == 0);
    REQUIRE(munmap(state, 4096) == 0);
    if (!guest)
        REQUIRE(unlink(filename) == 0);
    printf("SHARED kind=%s rounds=32\n", guest ? "anonymous-single-address-space" : "file");
}

int main(int argc, char **argv)
{
    REQUIRE(setvbuf(stdout, NULL, _IONBF, 0) == 0);
    executable = argv[0];
    if (argc >= 2 && !strcmp(argv[1], "--child"))
        return child(argc, argv);
    const char *test = argc == 2 ? argv[1] : getenv("PHASE1_CASE");
    REQUIRE(test != NULL);
    if (!strcmp(test, "hello")) {
        printf("HELLO Linux PIE pid=%ld\n", (long)getpid());
    } else if (!strcmp(test, "spawn")) {
        probe_spawn();
    } else if (!strcmp(test, "pipes")) {
        probe_pipes();
    } else if (!strcmp(test, "signals")) {
        probe_signals(0);
    } else if (!strcmp(test, "blocked-signals")) {
        probe_signals(1);
    } else if (!strcmp(test, "setsid")) {
        pid_t sid = getsid(0), pgrp = getpgrp();
        REQUIRE(sid >= 0 && pgrp >= 0);
        char *args[] = {(char *)executable, "--child", "setsid", NULL};
        wait_exit(spawn(args, NULL), 0);
        REQUIRE(getsid(0) == sid && getpgrp() == pgrp);
    } else if (!strcmp(test, "tls")) {
        tls_value = 77;
        unsigned values[] = {123, 125};
        pthread_t threads[2];
        for (int i = 0; i < 2; i++)
            REQUIRE(pthread_create(&threads[i], NULL, thread_tls, &values[i]) == 0);
        for (int i = 0; i < 2; i++)
            REQUIRE(pthread_join(threads[i], NULL) == 0);
        REQUIRE(tls_value == 77);
        char *args[] = {(char *)executable, "--child", "exit", NULL};
        wait_exit(spawn(args, NULL), 23);
        REQUIRE(tls_value == 77);
    } else if (!strcmp(test, "stack")) {
        check_stack();
        char *args[] = {(char *)executable, "--child", "stack", NULL};
        wait_exit(spawn(args, NULL), 0);
    } else if (!strcmp(test, "shared")) {
        probe_shared();
    } else {
        REQUIRE(!"unknown test");
    }
    printf("PASS %s\n", test);
    return 0;
}
