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

static void fail(const char *msg)
{
    fprintf(stderr, "abi probe failed: %s\n", msg);
    exit(1);
}

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

    puts("ok");
    return 0;
}
