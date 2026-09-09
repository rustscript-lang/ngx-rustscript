/* NGINX request bitfield accessors compiled against configured headers.
 *
 * nginx-sys bindgen bitfield offsets do not match the live GCC/Clang C ABI for
 * ngx_http_request_t / ngx_http_variable_value_t. Call these shims instead of
 * generated getters or setters.
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

void ngx_rustscript_c_request_set_uri_flags(ngx_http_request_t *r,
                                            unsigned int internal,
                                            unsigned int valid_unparsed_uri,
                                            unsigned int valid_location,
                                            unsigned int uri_changed)
{
    if (r == NULL) {
        return;
    }
    r->internal = internal ? 1 : 0;
    r->valid_unparsed_uri = valid_unparsed_uri ? 1 : 0;
    r->valid_location = valid_location ? 1 : 0;
    r->uri_changed = uri_changed ? 1 : 0;
}

void ngx_rustscript_c_request_set_valid_unparsed_uri(ngx_http_request_t *r,
                                                     unsigned int value)
{
    if (r == NULL) {
        return;
    }
    r->valid_unparsed_uri = value ? 1 : 0;
}

void ngx_rustscript_c_request_set_connection_type(ngx_http_request_t *r, unsigned int value)
{
    if (r == NULL) {
        return;
    }
    r->headers_in.connection_type = value;
}

void ngx_rustscript_c_headers_out_reset_specials(ngx_http_request_t *r)
{
    if (r == NULL) {
        return;
    }
    r->headers_out.content_type_len = 0;
    ngx_str_null(&r->headers_out.content_type);
    r->headers_out.content_type_lowcase = NULL;
    r->headers_out.content_length = NULL;
    r->headers_out.content_length_n = -1;
    r->headers_out.location = NULL;
    r->headers_out.last_modified = NULL;
    r->headers_out.last_modified_time = -1;
    r->headers_out.etag = NULL;
}

void ngx_rustscript_c_request_set_exten(ngx_http_request_t *r)
{
    if (r == NULL) {
        return;
    }
    ngx_http_set_exten(r);
}

unsigned int ngx_rustscript_c_variable_not_found(const ngx_http_variable_value_t *vv)
{
    if (vv == NULL) {
        return 1;
    }
    return vv->not_found != 0;
}

void ngx_rustscript_c_variable_set_flags(ngx_http_variable_value_t *vv,
                                         unsigned int valid,
                                         unsigned int not_found,
                                         unsigned int no_cacheable)
{
    if (vv == NULL) {
        return;
    }
    vv->valid = valid ? 1 : 0;
    vv->not_found = not_found ? 1 : 0;
    vv->no_cacheable = no_cacheable ? 1 : 0;
}

int ngx_rustscript_c_variable_set(ngx_http_request_t *r,
                                  u_char *name, size_t name_len,
                                  u_char *value, size_t value_len,
                                  int is_null)
{
    ngx_http_variable_t        *v;
    ngx_http_variable_value_t  *vv;
    ngx_http_core_main_conf_t  *cmcf;
    u_char                     *low;
    u_char                     *p;
    ngx_uint_t                  hash;

    if (r == NULL) {
        return 6;
    }
    if (r->pool == NULL) {
        return 5;
    }
    if (name == NULL || name_len == 0) {
        return 1;
    }

    low = ngx_pnalloc(r->pool, name_len);
    if (low == NULL) {
        return 4;
    }
    hash = ngx_hash_strlow(low, name, name_len);

    cmcf = ngx_http_get_module_main_conf(r, ngx_http_core_module);
    if (cmcf == NULL) {
        return 1;
    }

    v = ngx_hash_find(&cmcf->variables_hash, hash, low, name_len);
    if (v == NULL) {
        return 1;
    }
    if (!(v->flags & NGX_HTTP_VAR_CHANGEABLE)) {
        return 2;
    }

    if (v->set_handler) {
        vv = ngx_palloc(r->pool, sizeof(ngx_http_variable_value_t));
        if (vv == NULL) {
            return 4;
        }
        if (is_null) {
            vv->valid = 0;
            vv->not_found = 1;
            vv->no_cacheable = 0;
            vv->data = NULL;
            vv->len = 0;
        } else {
            if (value_len > 0) {
                p = ngx_pnalloc(r->pool, value_len);
                if (p == NULL) {
                    return 4;
                }
                ngx_memcpy(p, value, value_len);
            } else {
                p = (u_char *) "";
            }
            vv->valid = 1;
            vv->not_found = 0;
            vv->no_cacheable = 0;
            vv->data = p;
            vv->len = value_len;
        }
        v->set_handler(r, vv, v->data);
        return 0;
    }

    if (!(v->flags & NGX_HTTP_VAR_INDEXED)) {
        return 3;
    }
    if (r->variables == NULL) {
        return 3;
    }

    vv = &r->variables[v->index];
    if (is_null) {
        vv->valid = 0;
        vv->not_found = 1;
        vv->no_cacheable = 0;
        vv->data = NULL;
        vv->len = 0;
        return 0;
    }

    if (value_len > 0) {
        p = ngx_pnalloc(r->pool, value_len);
        if (p == NULL) {
            return 4;
        }
        ngx_memcpy(p, value, value_len);
    } else {
        p = (u_char *) "";
    }
    vv->valid = 1;
    vv->not_found = 0;
    vv->no_cacheable = 0;
    vv->data = p;
    vv->len = value_len;
    return 0;
}

time_t ngx_rustscript_c_parse_http_time(const u_char *data, size_t len)
{
    if (data == NULL || len == 0) {
        return (time_t)-1;
    }
    return ngx_http_parse_time((u_char *)data, len);
}

ngx_int_t ngx_rustscript_c_send_last_response(ngx_http_request_t *r, ngx_int_t status)
{
    ngx_int_t rc;

    if (r == NULL) {
        return NGX_ERROR;
    }
    if (status >= NGX_HTTP_OK
        && r->headers_out.status == 0
        && !r->header_sent)
    {
        r->headers_out.status = (ngx_uint_t) status;
    }
    if (r->header_sent) {
        return NGX_OK;
    }
    if (status < NGX_HTTP_OK
        || status >= NGX_HTTP_SPECIAL_RESPONSE
        || status == NGX_HTTP_CREATED
        || status == NGX_HTTP_NO_CONTENT)
    {
        return NGX_OK;
    }
    rc = ngx_http_send_header(r);
    if (rc == NGX_ERROR || rc > NGX_OK) {
        return rc;
    }
    return ngx_http_send_special(r, NGX_HTTP_LAST);
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
