/* macOS root launcher. No arbitrary commands, paths, environment or accounts. */
#include <bsm/audit.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <limits.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sysexits.h>
#include <unistd.h>

/* Packaging tests verify these fixed paths match knightbus_sdk::macos. */
#define DAEMON_PATH "/Library/Application Support/KnightBus/knightbusd"
#define OWNER_PATH "/var/db/knightbus/desktop-user"
#define STATE_PATH "/var/db/knightbus/identity"
#define LOG_PATH "/var/db/knightbus/service.log"
#define ENDPOINT_PATH "/var/run/knightbus/control"
#define MAX_UID_BYTES 10
#define NON_OWNER_ACCESS 0077
#define NON_OWNER_WRITE 0022
#define LOG_MODE 0600
#define ROOT_UID 0
#define DECIMAL_BASE 10

static bool trusted_file(int fd, mode_t forbidden) {
    struct stat info;
    return fd >= 0 && fstat(fd, &info) == 0 && S_ISREG(info.st_mode)
        && info.st_uid == ROOT_UID && !(info.st_mode & forbidden);
}

static bool installed_owner(uid_t caller) {
    int fd = open(OWNER_PATH, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (!trusted_file(fd, NON_OWNER_ACCESS)) {
        if (fd >= 0) close(fd);
        return false;
    }
    char bytes[MAX_UID_BYTES + 1];
    size_t count = 0;
    while (count < sizeof(bytes)) {
        ssize_t n = read(fd, bytes + count, sizeof(bytes) - count);
        if (n < 0 && errno == EINTR) continue;
        if (n < 0) { close(fd); return false; }
        if (!n) break;
        count += (size_t)n;
    }
    close(fd);
    if (!count || count > MAX_UID_BYTES) return false;
    uint64_t owner = 0;
    for (size_t i = 0; i < count; ++i) {
        if (bytes[i] < '0' || bytes[i] > '9') return false;
        owner = owner * DECIMAL_BASE + (unsigned)(bytes[i] - '0');
    }
    return owner <= UINT32_MAX && owner == caller;
}

static bool close_inherited_files(void) {
    DIR *files = opendir("/dev/fd");
    if (!files) return false;
    struct dirent *entry;
    while (true) {
        errno = 0;
        entry = readdir(files);
        if (!entry) {
            bool ok = errno == 0;
            closedir(files);
            return ok;
        }
        char *end;
        long fd = strtol(entry->d_name, &end, DECIMAL_BASE);
        if (!*end && fd > STDERR_FILENO && fd <= INT_MAX && fd != dirfd(files)) close((int)fd);
    }
}

int main(int argc, char **argv) {
    bool check = argc == 2 && strcmp(argv[1], "--check") == 0;
    if (argc != 1 && !check) return EX_USAGE;
    uid_t caller = getuid();
    au_id_t audit_user;
    /* Keep the actual login session; never manufacture an audit identity. */
    if (geteuid() != ROOT_UID || caller == ROOT_UID || getauid(&audit_user)
        || audit_user != caller || !installed_owner(caller)) return EX_NOPERM;
    int daemon = open(DAEMON_PATH, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    bool trusted = trusted_file(daemon, NON_OWNER_WRITE);
    if (daemon >= 0) close(daemon);
    if (!trusted) return EX_CONFIG;
    if (check) return EX_OK;

    int log = open(LOG_PATH, O_WRONLY | O_APPEND | O_CREAT | O_NOFOLLOW | O_CLOEXEC, LOG_MODE);
    if (!trusted_file(log, NON_OWNER_WRITE) || fchmod(log, LOG_MODE)) return EX_CANTCREAT;
    int input = open("/dev/null", O_RDONLY | O_CLOEXEC);
    if (input < 0 || dup2(input, STDIN_FILENO) < 0 || dup2(log, STDOUT_FILENO) < 0
        || dup2(log, STDERR_FILENO) < 0) return EX_OSERR;
    if (input > STDERR_FILENO) close(input);
    if (log > STDERR_FILENO) close(log);
    if (!close_inherited_files()) return EX_OSERR;
    umask(NON_OWNER_ACCESS);
    if (chdir("/") || setgroups(0, NULL) || setgid(ROOT_UID) || setuid(ROOT_UID)) return EX_NOPERM;
    char uid[MAX_UID_BYTES + 1];
    snprintf(uid, sizeof(uid), "%u", (unsigned)caller);
    char *const args[] = {DAEMON_PATH, "--desktop-app-user", STATE_PATH, ENDPOINT_PATH, uid, NULL};
    char *const environment[] = {NULL};
    /* exec preserves the app's responsibility and audit session. */
    execve(DAEMON_PATH, args, environment);
    return EX_OSERR;
}
