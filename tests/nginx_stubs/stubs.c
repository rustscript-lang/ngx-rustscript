/* Test-only NGINX C ABI. Exact fixed/variadic signatures; never success.
 *
 * Production cdylib must not include this file. Pointer-typed parameters use
 * the SysV pointer ABI (compatible with the nginx-sys bindgen types).
 * `ngx_err_t` is `int` and sits before the variadic `fmt` argument.
 *
 * Cycle-log tests observe ngx-rust `%*s` payloads through the capture helpers.
 */

#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>

typedef intptr_t ngx_int_t;
typedef uintptr_t ngx_uint_t;
typedef int ngx_err_t;

enum { NGX_ERROR = -1 };

ngx_int_t ngx_conf_full_name(void *cycle, void *name, ngx_uint_t conf_prefix)
{
    (void)cycle;
    (void)name;
    (void)conf_prefix;
    return (ngx_int_t)NGX_ERROR;
}

void ngx_conf_log_error(
    ngx_uint_t level,
    void *cf,
    ngx_err_t err,
    const char *fmt,
    ...
) {
    (void)level;
    (void)cf;
    (void)err;
    (void)fmt;
}

void *ngx_array_push(void *a)
{
    (void)a;
    return NULL;
}

enum { NGX_STUB_LOG_CAP = 4096 };

static char ngx_stub_last_log[NGX_STUB_LOG_CAP];
static int ngx_stub_log_count;

void ngx_rustscript_stub_log_reset(void)
{
    ngx_stub_log_count = 0;
    ngx_stub_last_log[0] = '\0';
}

int ngx_rustscript_stub_log_count(void)
{
    return ngx_stub_log_count;
}

const char *ngx_rustscript_stub_last_log(void)
{
    return ngx_stub_last_log;
}

void ngx_log_error_core(
    ngx_uint_t level,
    void *log,
    ngx_err_t err,
    const char *fmt,
    ...
) {
    va_list ap;

    ngx_stub_log_count++;
    ngx_stub_last_log[0] = '\0';

    if (fmt == NULL) {
        (void)level;
        (void)log;
        (void)err;
        return;
    }

    va_start(ap, fmt);
    /* ngx-rust formats with `%*s` and a `usize` length (see ngx::log::log_error). */
    if (strcmp(fmt, "%*s") == 0) {
        size_t len = va_arg(ap, size_t);
        const char *msg = va_arg(ap, const char *);
        if (msg != NULL && len > 0) {
            if (len >= sizeof(ngx_stub_last_log)) {
                len = sizeof(ngx_stub_last_log) - 1;
            }
            memcpy(ngx_stub_last_log, msg, len);
            ngx_stub_last_log[len] = '\0';
        }
    } else {
        vsnprintf(ngx_stub_last_log, sizeof(ngx_stub_last_log), fmt, ap);
    }
    va_end(ap);
    (void)level;
    (void)log;
    (void)err;
}

void *ngx_pool_cleanup_add(void *p, size_t size)
{
    (void)p;
    (void)size;
    return NULL;
}

void *ngx_palloc(void *pool, size_t size)
{
    (void)pool;
    (void)size;
    return NULL;
}

/* Zeroed inert object. Tests must not treat this as a live NGINX module. */
__attribute__((aligned(16))) unsigned char ngx_http_core_module[4096];

/*
 * Task 4 NGINX event/connection symbols. Production cdylib leaves these
 * undefined so nginx provides them. Stubs never succeed.
 */
void *ngx_get_connection(int s, void *log)
{
    (void)s;
    (void)log;
    return NULL;
}

void ngx_free_connection(void *c)
{
    (void)c;
}

void ngx_close_connection(void *c)
{
    (void)c;
}

__attribute__((aligned(8))) unsigned char ngx_event_actions[80];
uintptr_t ngx_event_flags;
uintptr_t ngx_process;
pid_t ngx_pid;
uintptr_t ngx_worker;
uintptr_t ngx_exiting;
void *ngx_cycle;
unsigned char ngx_core_module[4096];
ngx_int_t ngx_process_slot;
unsigned char ngx_processes[1024 * 256];

void ngx_http_core_run_phases(void *r)
{
    (void)r;
}

void ngx_http_test_reading(void *r)
{
    (void)r;
}

void ngx_http_block_reading(void *r)
{
    (void)r;
}

ngx_int_t ngx_handle_read_event(void *rev, uintptr_t flags)
{
    (void)rev;
    (void)flags;
    return (ngx_int_t)NGX_ERROR;
}

typedef struct {
    void *prev;
    void *next;
} ngx_queue_t;

ngx_queue_t ngx_posted_events;
