/* Test-only NGINX C ABI. Exact fixed/variadic signatures; never success.
 *
 * Production cdylib must not include this file. Pointer-typed parameters use
 * the SysV pointer ABI (compatible with the nginx-sys bindgen types).
 * `ngx_err_t` is `int` and sits before the variadic `fmt` argument.
 */

#include <stddef.h>
#include <stdint.h>

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

void ngx_log_error_core(
    ngx_uint_t level,
    void *log,
    ngx_err_t err,
    const char *fmt,
    ...
) {
    (void)level;
    (void)log;
    (void)err;
    (void)fmt;
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

void ngx_http_core_run_phases(void *r)
{
    (void)r;
}
