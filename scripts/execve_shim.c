/*
 * Hagibis execve shim for LD_PRELOAD
 * Intercepts execve calls for profiling, sandboxing, and execution tracing.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <unistd.h>
#include <stdlib.h>

static int (*real_execve)(const char *pathname, char *const argv[], char *const envp[]) = NULL;

int execve(const char *pathname, char *const argv[], char *const envp[]) {
    if (!real_execve) {
        real_execve = dlsym(RTLD_NEXT, "execve");
    }
    return real_execve(pathname, argv, envp);
}
