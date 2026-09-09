/* Read-only NGINX request bitfield accessors compiled against configured headers.
 *
 * nginx-sys bindgen bitfield offsets do not match the live GCC/Clang C ABI for
 * ngx_http_request_t. Call these shims instead of generated getters.
 *
 * Production Rust re-exports `ngx_rustscript_request_*` with #[no_mangle].
 * Standalone C probes compile this file without NGX_RUSTSCRIPT_NO_C_PUBLIC_ALIAS
 * so the public names exist in C as well.
 */

#include <ngx_config.h>
#include <ngx_core.h>
#include <ngx_http.h>

unsigned int ngx_rustscript_c_request_is_internal(const ngx_http_request_t *r)
{
    if (r == NULL) {
        return 0;
    }
    return r->internal != 0;
}

unsigned int ngx_rustscript_c_request_headers_sent(const ngx_http_request_t *r)
{
    if (r == NULL) {
        return 0;
    }
    return r->header_sent != 0;
}

unsigned int ngx_rustscript_c_request_reading_body(const ngx_http_request_t *r)
{
    if (r == NULL) {
        return 0;
    }
    return r->reading_body != 0;
}

unsigned int ngx_rustscript_c_request_discard_body(const ngx_http_request_t *r)
{
    if (r == NULL) {
        return 0;
    }
    return r->discard_body != 0;
}

#ifndef NGX_RUSTSCRIPT_NO_C_PUBLIC_ALIAS
unsigned int ngx_rustscript_request_is_internal(const ngx_http_request_t *r)
{
    return ngx_rustscript_c_request_is_internal(r);
}

unsigned int ngx_rustscript_request_headers_sent(const ngx_http_request_t *r)
{
    return ngx_rustscript_c_request_headers_sent(r);
}

unsigned int ngx_rustscript_request_reading_body(const ngx_http_request_t *r)
{
    return ngx_rustscript_c_request_reading_body(r);
}

unsigned int ngx_rustscript_request_discard_body(const ngx_http_request_t *r)
{
    return ngx_rustscript_c_request_discard_body(r);
}
#endif
