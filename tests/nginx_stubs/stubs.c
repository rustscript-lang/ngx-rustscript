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
#include <time.h>

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

/* ngx_palloc / ngx_pnalloc / ngx_list_push live in request_abi.c so they can
 * use official NGINX list/pool layouts and a test-only fail-after-N allocator. */

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

typedef struct {
    size_t len;
    unsigned char *data;
} ngx_str_t;

typedef struct {
    unsigned len:28;
    unsigned valid:1;
    unsigned no_cacheable:1;
    unsigned not_found:1;
    unsigned escape:1;
    unsigned char *data;
} ngx_http_variable_value_t;

ngx_http_variable_value_t *ngx_http_get_variable(void *r, ngx_str_t *name, ngx_uint_t key)
{
    static ngx_http_variable_value_t vv;
    static unsigned char found[] = "stub-value";
    static unsigned char empty[] = "";

    (void)r;
    (void)key;
    if (name == NULL || name->data == NULL) {
        return NULL;
    }
    if (name->len == 5 && memcmp(name->data, "empty", 5) == 0) {
        memset(&vv, 0, sizeof(vv));
        vv.valid = 1;
        vv.len = 0;
        vv.data = empty;
        return &vv;
    }
    if (name->len == 5 && memcmp(name->data, "found", 5) == 0) {
        memset(&vv, 0, sizeof(vv));
        vv.valid = 1;
        vv.len = 10;
        vv.data = found;
        return &vv;
    }
    return NULL;
}

void *(*ngx_rustscript_stub_hash_find_hook)(void *hash, ngx_uint_t key, unsigned char *name, size_t len);

void *ngx_hash_find(void *hash, ngx_uint_t key, unsigned char *name, size_t len)
{
    if (ngx_rustscript_stub_hash_find_hook != NULL) {
        return ngx_rustscript_stub_hash_find_hook(hash, key, name, len);
    }
    (void)hash;
    (void)key;
    (void)name;
    (void)len;
    return NULL;
}

ngx_uint_t ngx_hash_strlow(unsigned char *dst, unsigned char *src, size_t n)
{
    ngx_uint_t key = 0;
    size_t i;

    for (i = 0; i < n; i++) {
        unsigned char c = src[i];
        if (c >= 'A' && c <= 'Z') {
            c = (unsigned char)(c | 0x20);
        }
        dst[i] = c;
        key = key * 31 + c;
    }
    return key;
}

void ngx_http_set_exten(void *r)
{
    (void)r;
}

time_t ngx_parse_http_time(unsigned char *value, size_t len)
{
    (void)value;
    (void)len;
    return (time_t)-1;
}

/* ngx_http_send_header / ngx_http_send_special live in request_abi.c. */
