/* Standalone GCC/Clang probe: set C ngx_http_request_t bitfields through the
 * official headers and assert the production shims follow those fields.
 * Compile together with src/nginx_request_shim.c. Never uses bindgen.
 */

#include <ngx_config.h>
#include <ngx_core.h>
#include <ngx_http.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

unsigned int ngx_rustscript_request_is_internal(const ngx_http_request_t *r);
unsigned int ngx_rustscript_request_headers_sent(const ngx_http_request_t *r);
unsigned int ngx_rustscript_request_reading_body(const ngx_http_request_t *r);
unsigned int ngx_rustscript_request_discard_body(const ngx_http_request_t *r);
void ngx_rustscript_c_request_set_uri_flags(ngx_http_request_t *r, unsigned int internal,
    unsigned int valid_unparsed_uri, unsigned int valid_location, unsigned int uri_changed);
void ngx_rustscript_c_headers_out_reset_specials(ngx_http_request_t *r);
time_t ngx_rustscript_c_parse_http_time(const u_char *data, size_t len);
unsigned int ngx_rustscript_c_variable_not_found(const ngx_http_variable_value_t *vv);

static void fail(const char *msg)
{
    fprintf(stderr, "abi probe failed: %s\n", msg);
    exit(1);
}

ngx_module_t ngx_http_core_module;

void ngx_http_set_exten(ngx_http_request_t *r)
{
    (void)r;
}

void *ngx_pnalloc(ngx_pool_t *pool, size_t size)
{
    (void)pool;
    (void)size;
    return NULL;
}

void *ngx_palloc(ngx_pool_t *pool, size_t size)
{
    (void)pool;
    (void)size;
    return NULL;
}

ngx_uint_t ngx_hash_strlow(u_char *dst, u_char *src, size_t n)
{
    (void)dst;
    (void)src;
    (void)n;
    return 0;
}

void *ngx_hash_find(ngx_hash_t *hash, ngx_uint_t key, u_char *name, size_t len)
{
    (void)hash;
    (void)key;
    (void)name;
    (void)len;
    return NULL;
}

time_t ngx_parse_http_time(u_char *value, size_t len)
{
    (void)value;
    (void)len;
    return (time_t)-1;
}

ngx_int_t ngx_http_send_header(ngx_http_request_t *r)
{
    (void)r;
    return NGX_OK;
}

ngx_int_t ngx_http_send_special(ngx_http_request_t *r, ngx_uint_t flags)
{
    (void)r;
    (void)flags;
    return NGX_OK;
}

off_t ngx_atoof(u_char *line, size_t n)
{
    off_t value = 0;

    if (n == 0) {
        return NGX_ERROR;
    }
    for (; n--; line++) {
        if (*line < '0' || *line > '9') {
            return NGX_ERROR;
        }
        value = value * 10 + (*line - '0');
    }
    return value;
}

void ngx_strlow(u_char *dst, u_char *src, size_t n)
{
    while (n) {
        *dst = ngx_tolower(*src);
        dst++;
        src++;
        n--;
    }
}

ngx_int_t ngx_rustscript_c_resp_status(const ngx_http_request_t *r);
void ngx_rustscript_c_headers_in_apply_user_agent(ngx_http_request_t *r, ngx_table_elt_t *h);
ngx_int_t ngx_rustscript_c_validate_host(ngx_pool_t *pool, u_char *data, size_t len, ngx_str_t *out);
ngx_int_t ngx_rustscript_c_parse_content_length(const u_char *data, size_t len, off_t *out);

int main(void)
{
    ngx_http_request_t r;

    if (sizeof(r) != 1408) {
        fprintf(stderr, "sizeof(ngx_http_request_t)=%zu\n", sizeof(r));
        fail("sizeof(ngx_http_request_t) != 1408");
    }
    if (ngx_rustscript_request_is_internal(NULL) != 0
        || ngx_rustscript_request_headers_sent(NULL) != 0
        || ngx_rustscript_request_reading_body(NULL) != 0
        || ngx_rustscript_request_discard_body(NULL) != 0)
    {
        fail("NULL request must read as false");
    }

    memset(&r, 0, sizeof(r));
    if (ngx_rustscript_request_is_internal(&r) != 0
        || ngx_rustscript_request_headers_sent(&r) != 0
        || ngx_rustscript_request_reading_body(&r) != 0
        || ngx_rustscript_request_discard_body(&r) != 0)
    {
        fail("zeroed request must read as false");
    }

    r.filter_finalize = 1;
    if (ngx_rustscript_request_is_internal(&r) != 0) {
        fail("C filter_finalize must not make is_internal true");
    }
    r.filter_finalize = 0;

    r.error_page = 1;
    if (ngx_rustscript_request_is_internal(&r) != 0) {
        fail("C error_page must not make is_internal true");
    }
    r.error_page = 0;

    r.internal = 1;
    if (ngx_rustscript_request_is_internal(&r) != 1) {
        fail("C internal=1 must make is_internal true");
    }
    if (ngx_rustscript_request_headers_sent(&r) != 0
        || ngx_rustscript_request_reading_body(&r) != 0
        || ngx_rustscript_request_discard_body(&r) != 0)
    {
        fail("C internal must not set neighboring shims");
    }
    r.internal = 0;

    r.header_sent = 1;
    if (ngx_rustscript_request_headers_sent(&r) != 1) {
        fail("C header_sent=1 must make headers_sent true");
    }
    if (ngx_rustscript_request_is_internal(&r) != 0) {
        fail("C header_sent must not make is_internal true");
    }
    r.header_sent = 0;

    r.reading_body = 1;
    if (ngx_rustscript_request_reading_body(&r) != 1) {
        fail("C reading_body=1 must make reading_body true");
    }
    if (ngx_rustscript_request_is_internal(&r) != 0
        || ngx_rustscript_request_discard_body(&r) != 0)
    {
        fail("C reading_body must not set neighboring shims");
    }
    r.reading_body = 0;

    r.discard_body = 1;
    if (ngx_rustscript_request_discard_body(&r) != 1) {
        fail("C discard_body=1 must make discard_body true");
    }
    if (ngx_rustscript_request_is_internal(&r) != 0) {
        fail("C discard_body must not make is_internal true");
    }

    r.discard_body = 0;

    ngx_rustscript_c_request_set_uri_flags(NULL, 1, 1, 1, 1);
    ngx_rustscript_c_headers_out_reset_specials(NULL);
    if (ngx_rustscript_c_parse_http_time(NULL, 4) != (time_t)-1) {
        fail("NULL parse_http_time must return -1");
    }
    if (ngx_rustscript_c_variable_not_found(NULL) != 1) {
        fail("NULL variable_not_found must return 1");
    }

    r.filter_finalize = 1;
    ngx_rustscript_c_request_set_uri_flags(&r, 1, 0, 0, 0);
    if (!r.internal) {
        fail("set_uri_flags internal=1 must set internal");
    }
    if (r.valid_unparsed_uri || r.valid_location || r.uri_changed) {
        fail("set_uri_flags must clear the requested uri flags");
    }
    if (!r.filter_finalize) {
        fail("set_uri_flags must not clobber adjacent filter_finalize");
    }

    r.err_status = 500;
    r.headers_out.status = 200;
    r.http_version = NGX_HTTP_VERSION_11;
    if (ngx_rustscript_c_resp_status(&r) != 500) {
        fail("resp_status must prefer err_status");
    }
    r.err_status = 0;
    if (ngx_rustscript_c_resp_status(&r) != 200) {
        fail("resp_status must use headers_out.status");
    }
    r.headers_out.status = 0;
    r.http_version = NGX_HTTP_VERSION_9;
    if (ngx_rustscript_c_resp_status(&r) != 9) {
        fail("HTTP/0.9 must map to 9");
    }

    {
        ngx_table_elt_t ua;
        u_char value[] = "Mozilla/4.0 (compatible; MSIE 6.0; Windows NT 5.1)";
        memset(&ua, 0, sizeof(ua));
        ua.value.data = value;
        ua.value.len = sizeof(value) - 1;
        r.headers_in.opera = 1;
        ngx_rustscript_c_headers_in_apply_user_agent(&r, &ua);
        if (!r.headers_in.msie || !r.headers_in.msie6) {
            fail("UA shim must set msie/msie6");
        }
        if (r.headers_in.opera) {
            fail("MSIE UA must clear opera");
        }
        ngx_rustscript_c_headers_in_apply_user_agent(&r, NULL);
        if (r.headers_in.msie || r.headers_in.msie6) {
            fail("NULL UA must clear flags");
        }
    }

    if (ngx_rustscript_c_validate_host(NULL, NULL, 0, NULL) != NGX_ERROR) {
        fail("NULL host must be invalid");
    }
    {
        off_t n = 7;
        if (ngx_rustscript_c_parse_content_length((u_char *)"abc", 3, &n) != NGX_ERROR
            || n != 7)
        {
            fail("invalid Content-Length must not write out");
        }
        if (ngx_rustscript_c_parse_content_length((u_char *)"42", 2, &n) != NGX_OK
            || n != 42)
        {
            fail("valid Content-Length must parse");
        }
    }

    puts("ok");
    return 0;
}
