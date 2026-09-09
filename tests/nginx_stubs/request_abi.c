/* Test-only NGINX request bitfield setters compiled against official headers.
 *
 * Production cdylib must not include this file. Setters write the live C ABI
 * so cargo tests can detect bindgen bitfield layout mismatches.
 */

#include <ngx_config.h>
#include <ngx_core.h>
#include <ngx_http.h>

#include <stdlib.h>

ngx_http_request_t *ngx_rustscript_test_request_new(void)
{
    ngx_http_request_t *r;

    r = calloc(1, sizeof(*r));
    if (r == NULL) {
        return NULL;
    }
    r->main = r;
    return r;
}

void ngx_rustscript_test_request_free(ngx_http_request_t *r)
{
    free(r);
}

unsigned int ngx_rustscript_test_request_sizeof(void)
{
    return (unsigned int)sizeof(ngx_http_request_t);
}

void ngx_rustscript_test_request_set_internal(ngx_http_request_t *r, unsigned int val)
{
    if (r == NULL) {
        return;
    }
    r->internal = val != 0;
}

void ngx_rustscript_test_request_set_filter_finalize(ngx_http_request_t *r, unsigned int val)
{
    if (r == NULL) {
        return;
    }
    r->filter_finalize = val != 0;
}

void ngx_rustscript_test_request_set_error_page(ngx_http_request_t *r, unsigned int val)
{
    if (r == NULL) {
        return;
    }
    r->error_page = val != 0;
}

void ngx_rustscript_test_request_set_header_sent(ngx_http_request_t *r, unsigned int val)
{
    if (r == NULL) {
        return;
    }
    r->header_sent = val != 0;
}

void ngx_rustscript_test_request_set_reading_body(ngx_http_request_t *r, unsigned int val)
{
    if (r == NULL) {
        return;
    }
    r->reading_body = val != 0;
}

void ngx_rustscript_test_request_set_discard_body(ngx_http_request_t *r, unsigned int val)
{
    if (r == NULL) {
        return;
    }
    r->discard_body = val != 0;
}
