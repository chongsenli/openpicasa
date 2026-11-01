/*
 * ro_guard_preload.so —— R-1「照片目录零写入」的 libc 拦截器。
 *
 * 为什么不用 strace：ptrace 在很多开发环境里被禁（YAMA ptrace_scope、容器 seccomp、
 * 沙箱）。`LD_PRELOAD` **不需要任何特权**，而 Rust 的 `std::fs` 最终都走 libc，
 * 所以覆盖是完整的。代价是拦不到绕过 libc 的裸 syscall —— 本项目不存在这种调用。
 *
 * 用法（由 tools/ro_guard.sh 调用）：
 *   RO_GUARD_DIR=/照片目录 RO_GUARD_LOG=/tmp/violations.log \
 *     LD_PRELOAD=./ro_guard_preload.so <被测程序> ...
 *
 * 判定：凡**路径落在 RO_GUARD_DIR 内**且属于下面任一类，就写一行到 RO_GUARD_LOG：
 *   ① 以可写方式打开（O_WRONLY/O_RDWR/O_CREAT/O_TRUNC/O_APPEND）
 *   ② 写（write/pwrite/writev，fd 通过 /proc/self/fd 反解成路径）
 *   ③ 结构性改动（unlink/rename/mkdir/rmdir/truncate/…）
 */

#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static const char *WATCH = NULL;
static const char *LOGP = NULL;
static const char *LOGALL = NULL;
static int guard_ready = 0;

typedef int (*openat_fn)(int, const char *, int, ...);
typedef int (*open_fn)(const char *, int, ...);
typedef int (*creat_fn)(const char *, mode_t);
typedef ssize_t (*write_fn)(int, const void *, size_t);
typedef ssize_t (*pwrite_fn)(int, const void *, size_t, off_t);
typedef ssize_t (*writev_fn)(int, const struct iovec *, int);
typedef int (*unlink_fn)(const char *);
typedef int (*unlinkat_fn)(int, const char *, int);
typedef int (*rename_fn)(const char *, const char *);
typedef int (*renameat_fn)(int, const char *, int, const char *, unsigned);
typedef int (*mkdir_fn)(const char *, mode_t);
typedef int (*rmdir_fn)(const char *);
typedef int (*truncate_fn)(const char *, off_t);
typedef int (*chmod_fn)(const char *, mode_t);
typedef int (*chown_fn)(const char *, uid_t, gid_t);
typedef int (*utimes_fn)(const char *, const struct timeval *);
typedef int (*symlink_fn)(const char *, const char *);
typedef int (*link_fn)(const char *, const char *);
typedef int (*remove_fn)(const char *);
typedef FILE *(*fopen_fn)(const char *, const char *);

static openat_fn real_openat;
static open_fn real_open;
static open_fn real_open64;
static creat_fn real_creat;
static creat_fn real_creat64;
static write_fn real_write;
static pwrite_fn real_pwrite;
static pwrite_fn real_pwrite64;
static writev_fn real_writev;
static unlink_fn real_unlink;
static unlinkat_fn real_unlinkat;
static rename_fn real_rename;
static renameat_fn real_renameat;
static renameat_fn real_renameat2;
static mkdir_fn real_mkdir;
static rmdir_fn real_rmdir;
static truncate_fn real_truncate;
static chmod_fn real_chmod;
static chown_fn real_chown;
static utimes_fn real_utimes;
static symlink_fn real_symlink;
static link_fn real_link;
static remove_fn real_remove;
static fopen_fn real_fopen;

#define RESOLVE(var, name)                       \
  do {                                           \
    if (!(var)) (var) = (typeof(var))dlsym(RTLD_NEXT, name); \
  } while (0)

__attribute__((constructor)) static void ro_guard_init(void) {
  WATCH = getenv("RO_GUARD_DIR");
  LOGP = getenv("RO_GUARD_LOG");
  LOGALL = getenv("RO_GUARD_LOG_ALL");
  if (!WATCH || !LOGP || !*WATCH) {
    return; /* 未配置 → 完全不介入 */
  }
  RESOLVE(real_openat, "openat");
  RESOLVE(real_open, "open");
  RESOLVE(real_open64, "open64");
  RESOLVE(real_creat, "creat");
  RESOLVE(real_creat64, "creat64");
  RESOLVE(real_write, "write");
  RESOLVE(real_pwrite, "pwrite");
  RESOLVE(real_pwrite64, "pwrite64");
  RESOLVE(real_writev, "writev");
  RESOLVE(real_unlink, "unlink");
  RESOLVE(real_unlinkat, "unlinkat");
  RESOLVE(real_rename, "rename");
  RESOLVE(real_renameat, "renameat");
  RESOLVE(real_renameat2, "renameat2");
  RESOLVE(real_mkdir, "mkdir");
  RESOLVE(real_rmdir, "rmdir");
  RESOLVE(real_truncate, "truncate");
  RESOLVE(real_chmod, "chmod");
  RESOLVE(real_chown, "chown");
  RESOLVE(real_utimes, "utimes");
  RESOLVE(real_symlink, "symlink");
  RESOLVE(real_link, "link");
  RESOLVE(real_remove, "remove");
  RESOLVE(real_fopen, "fopen");
  guard_ready = (real_openat && real_write) ? 1 : 0;
}

/* 记一条违规。全程只用 real_* ，绝不递归回自己的拦截函数。 */
static void report(const char *op, const char *path) {
  if (!guard_ready) return;
  int fd = real_openat(AT_FDCWD, LOGP, O_WRONLY | O_CREAT | O_APPEND, 0644);
  if (fd < 0) return;
  char buf[4096];
  int n = snprintf(buf, sizeof(buf), "%s\t%s\n", op, path ? path : "(null)");
  if (n > 0) real_write(fd, buf, (size_t)n < sizeof(buf) ? (size_t)n : sizeof(buf));
  close(fd);
}

/*
 * 记一次「碰到了被监视目录」（含只读）。
 *
 * 为什么需要它：没有这一条，"零违规"可能只是"压根没读到照片目录"——
 * 那样的 GUARD 判定是**假通过**。判定脚本会要求这个计数 > 0。
 */
static void log_access(const char *op, const char *path) {
  if (!guard_ready || !LOGALL || !*LOGALL) return;
  int fd = real_openat(AT_FDCWD, LOGALL, O_WRONLY | O_CREAT | O_APPEND, 0644);
  if (fd < 0) return;
  char buf[4096];
  int n = snprintf(buf, sizeof(buf), "%s\t%s\n", op, path ? path : "(null)");
  if (n > 0) real_write(fd, buf, (size_t)n < sizeof(buf) ? (size_t)n : sizeof(buf));
  close(fd);
}

/* 路径是否落在被监视目录内 */
static int watched(const char *path) {
  if (!guard_ready || !path || !*path) return 0;
  if (path[0] != '/') return 0; /* 相对路径不判断（被测程序用的是绝对路径） */
  size_t n = strlen(WATCH);
  if (strncmp(path, WATCH, n) != 0) return 0;
  return path[n] == '\0' || path[n] == '/';
}

static int flags_write(int flags) {
  return (flags & O_WRONLY) || (flags & O_RDWR) || (flags & O_CREAT) ||
         (flags & O_TRUNC) || (flags & O_APPEND);
}

/* 由 fd 反解路径（write/pwrite/writev 只有 fd） */
static void path_of_fd(int fd, char *out, size_t cap) {
  char link[64];
  snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
  ssize_t n = readlink(link, out, cap - 1);
  if (n < 0) {
    out[0] = '\0';
    return;
  }
  out[n] = '\0';
}

/* ------------------------------------------------------------ 拦截点 */

int openat(int dirfd, const char *path, int flags, ...) {
  RESOLVE(real_openat, "openat");
  mode_t mode = 0;
  if (flags & (O_CREAT | O_TMPFILE)) {
    va_list ap;
    va_start(ap, flags);
    mode = va_arg(ap, mode_t);
    va_end(ap);
  }
  int r = real_openat(dirfd, path, flags, mode);
  if (r >= 0 && watched(path)) {
    log_access("openat", path);
    if (flags_write(flags)) report("openat(write)", path);
  }
  return r;
}

int open(const char *path, int flags, ...) {
  RESOLVE(real_open, "open");
  mode_t mode = 0;
  if (flags & (O_CREAT | O_TMPFILE)) {
    va_list ap;
    va_start(ap, flags);
    mode = va_arg(ap, mode_t);
    va_end(ap);
  }
  int r = real_open(path, flags, mode);
  if (r >= 0 && watched(path)) {
    log_access("open", path);
    if (flags_write(flags)) report("open(write)", path);
  }
  return r;
}

int open64(const char *path, int flags, ...) {
  RESOLVE(real_open64, "open64");
  mode_t mode = 0;
  if (flags & (O_CREAT | O_TMPFILE)) {
    va_list ap;
    va_start(ap, flags);
    mode = va_arg(ap, mode_t);
    va_end(ap);
  }
  int r = real_open64(path, flags, mode);
  if (r >= 0 && watched(path)) {
    log_access("open64", path);
    if (flags_write(flags)) report("open64(write)", path);
  }
  return r;
}

int creat(const char *path, mode_t mode) {
  RESOLVE(real_creat, "creat");
  if (watched(path)) report("creat", path);
  return real_creat(path, mode);
}

int creat64(const char *path, mode_t mode) {
  RESOLVE(real_creat64, "creat64");
  if (watched(path)) report("creat64", path);
  return real_creat64(path, mode);
}

FILE *fopen(const char *path, const char *mode) {
  RESOLVE(real_fopen, "fopen");
  if (watched(path) && mode && (strchr(mode, 'w') || strchr(mode, 'a') || strchr(mode, '+'))) {
    report("fopen(write)", path);
  }
  return real_fopen(path, mode);
}

static void check_fd_write(const char *op, int fd) {
  if (!guard_ready || fd < 0) return;
  char p[4096];
  path_of_fd(fd, p, sizeof(p));
  if (watched(p)) report(op, p);
}

ssize_t write(int fd, const void *buf, size_t n) {
  RESOLVE(real_write, "write");
  if (guard_ready && fd > 2) check_fd_write("write", fd);
  return real_write(fd, buf, n);
}

ssize_t pwrite(int fd, const void *buf, size_t n, off_t off) {
  RESOLVE(real_pwrite, "pwrite");
  if (guard_ready && fd > 2) check_fd_write("pwrite", fd);
  return real_pwrite(fd, buf, n, off);
}

ssize_t pwrite64(int fd, const void *buf, size_t n, off_t off) {
  RESOLVE(real_pwrite64, "pwrite64");
  if (guard_ready && fd > 2) check_fd_write("pwrite64", fd);
  return real_pwrite64(fd, buf, n, off);
}

ssize_t writev(int fd, const struct iovec *iov, int cnt) {
  RESOLVE(real_writev, "writev");
  if (guard_ready && fd > 2) check_fd_write("writev", fd);
  return real_writev(fd, iov, cnt);
}

int unlink(const char *path) {
  RESOLVE(real_unlink, "unlink");
  if (watched(path)) report("unlink", path);
  return real_unlink(path);
}

int unlinkat(int dirfd, const char *path, int flags) {
  RESOLVE(real_unlinkat, "unlinkat");
  if (watched(path)) report("unlinkat", path);
  return real_unlinkat(dirfd, path, flags);
}

int rename(const char *a, const char *b) {
  RESOLVE(real_rename, "rename");
  if (watched(a)) report("rename(from)", a);
  if (watched(b)) report("rename(to)", b);
  return real_rename(a, b);
}

int renameat(int fd1, const char *a, int fd2, const char *b) {
  RESOLVE(real_renameat, "renameat");
  if (watched(a)) report("renameat(from)", a);
  if (watched(b)) report("renameat(to)", b);
  return real_renameat(fd1, a, fd2, b, 0);
}

int renameat2(int fd1, const char *a, int fd2, const char *b, unsigned flags) {
  RESOLVE(real_renameat2, "renameat2");
  if (watched(a)) report("renameat2(from)", a);
  if (watched(b)) report("renameat2(to)", b);
  return real_renameat2(fd1, a, fd2, b, flags);
}

int mkdir(const char *path, mode_t mode) {
  RESOLVE(real_mkdir, "mkdir");
  if (watched(path)) report("mkdir", path);
  return real_mkdir(path, mode);
}

int rmdir(const char *path) {
  RESOLVE(real_rmdir, "rmdir");
  if (watched(path)) report("rmdir", path);
  return real_rmdir(path);
}

int truncate(const char *path, off_t len) {
  RESOLVE(real_truncate, "truncate");
  if (watched(path)) report("truncate", path);
  return real_truncate(path, len);
}

int chmod(const char *path, mode_t mode) {
  RESOLVE(real_chmod, "chmod");
  if (watched(path)) report("chmod", path);
  return real_chmod(path, mode);
}

int chown(const char *path, uid_t u, gid_t g) {
  RESOLVE(real_chown, "chown");
  if (watched(path)) report("chown", path);
  return real_chown(path, u, g);
}

int utimes(const char *path, const struct timeval *tv) {
  RESOLVE(real_utimes, "utimes");
  if (watched(path)) report("utimes", path);
  return real_utimes(path, tv);
}

int symlink(const char *t, const char *p) {
  RESOLVE(real_symlink, "symlink");
  if (watched(p)) report("symlink", p);
  return real_symlink(t, p);
}

int link(const char *a, const char *b) {
  RESOLVE(real_link, "link");
  if (watched(b)) report("link", b);
  return real_link(a, b);
}

int remove(const char *path) {
  RESOLVE(real_remove, "remove");
  if (watched(path)) report("remove", path);
  return real_remove(path);
}
