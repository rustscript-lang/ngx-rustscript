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

ngx_int_t ngx_rustscript_c_resp_status(const ngx_http_request_t *r)
{
    if (r == NULL) {
        return 0;
    }
    if (r->err_status) {
        return (ngx_int_t) r->err_status;
    }
    if (r->headers_out.status) {
        return (ngx_int_t) r->headers_out.status;
    }
    if (r->http_version == NGX_HTTP_VERSION_9) {
        return 9;
    }
    return 0;
}

ngx_int_t ngx_rustscript_c_parse_content_length(const u_char *data, size_t len, off_t *out)
{
    off_t n;

    if (out == NULL) {
        return NGX_ERROR;
    }
    if (data == NULL || len == 0) {
        return NGX_ERROR;
    }
    n = ngx_atoof((u_char *) data, len);
    if (n == NGX_ERROR) {
        return NGX_ERROR;
    }
    *out = n;
    return NGX_OK;
}

static ngx_int_t
ngx_rustscript_validate_host_copy(ngx_str_t *host, ngx_pool_t *pool, ngx_uint_t alloc)
{
    u_char  *h, ch;
    size_t   i, dot_pos, host_len;
    enum {
        sw_usual = 0,
        sw_literal,
        sw_rest
    } state;

    if (host == NULL || host->data == NULL || host->len == 0) {
        return NGX_DECLINED;
    }

    dot_pos = host->len;
    host_len = host->len;

    h = host->data;

    state = sw_usual;

    for (i = 0; i < host->len; i++) {
        ch = h[i];

        if (ch == '.') {
            if (dot_pos == i - 1) {
                return NGX_DECLINED;
            }
            dot_pos = i;
        }

        switch (state) {
        case sw_usual:
            if (alloc == 0) {
                if (ch >= 'A' && ch <= 'Z') {
                    alloc = 1;
                }
            }
            if (ch == ':') {
                host_len = i;
                state = sw_rest;
            } else if (ch == '[') {
                state = sw_literal;
            }
            break;

        case sw_literal:
            if (ch >= 'A' && ch <= 'Z') {
                alloc = 1;
            }
            if (ch == ']') {
                host_len = i + 1;
                state = sw_rest;
            }
            break;

        case sw_rest:
            if (ch >= 'A' && ch <= 'Z') {
                alloc = 1;
            }
            break;
        }

        if (ngx_path_separator(ch) || ch == '\0') {
            return NGX_DECLINED;
        }
    }

    if (dot_pos == host_len - 1) {
        host_len--;
    }

    if (host_len == 0) {
        return NGX_DECLINED;
    }

    host->len = host_len;

    if (alloc) {
        if (pool == NULL) {
            return NGX_ERROR;
        }
        host->data = ngx_pnalloc(pool, host_len);
        if (host->data == NULL) {
            return NGX_ERROR;
        }
        ngx_strlow(host->data, h, host_len);
    }

    return NGX_OK;
}

ngx_int_t ngx_rustscript_c_validate_host(ngx_pool_t *pool, u_char *data, size_t len,
    ngx_str_t *server_out)
{
    ngx_str_t host;
    ngx_int_t rc;

    if (data == NULL || len == 0 || server_out == NULL) {
        return NGX_ERROR;
    }
    host.data = data;
    host.len = len;
    rc = ngx_rustscript_validate_host_copy(&host, pool, 0);
    if (rc != NGX_OK) {
        return NGX_ERROR;
    }
    *server_out = host;
    return NGX_OK;
}

void ngx_rustscript_c_invalidate_cached_host(ngx_http_request_t *r)
{
    ngx_http_core_main_conf_t *cmcf;
    ngx_http_variable_t       *v;
    ngx_uint_t                 hash;
    u_char                     low[sizeof("host") - 1];

    if (r == NULL || r->variables == NULL) {
        return;
    }
    cmcf = ngx_http_get_module_main_conf(r, ngx_http_core_module);
    if (cmcf == NULL || cmcf->variables.elts == NULL) {
        return;
    }
    hash = ngx_hash_strlow(low, (u_char *) "host", sizeof(low));
    v = ngx_hash_find(&cmcf->variables_hash, hash, low, sizeof(low));
    if (v == NULL || !(v->flags & NGX_HTTP_VAR_INDEXED)) {
        return;
    }
    if (v->index >= cmcf->variables.nelts) {
        return;
    }
    r->variables[v->index].valid = 0;
    r->variables[v->index].not_found = 0;
}

static u_char *
ngx_rustscript_substr(u_char *s, size_t len, const char *needle, size_t nlen)
{
    size_t i;

    if (s == NULL || needle == NULL || nlen == 0 || len < nlen) {
        return NULL;
    }
    for (i = 0; i + nlen <= len; i++) {
        if (memcmp(s + i, needle, nlen) == 0) {
            return s + i;
        }
    }
    return NULL;
}

void ngx_rustscript_c_headers_in_apply_user_agent(ngx_http_request_t *r, ngx_table_elt_t *h)
{
    u_char   *user_agent, *msie;
    size_t    ua_len;
    unsigned  msie_f = 0, msie6 = 0, opera = 0, gecko = 0, chrome = 0, safari = 0, konqueror = 0;

    if (r == NULL) {
        return;
    }

    r->headers_in.user_agent = h;

    if (h != NULL && h->value.data != NULL && h->value.len > 0) {
        user_agent = h->value.data;
        ua_len = h->value.len;

        msie = ngx_rustscript_substr(user_agent, ua_len, "MSIE ", 5);
        if (msie && msie + 7 < user_agent + ua_len) {
            msie_f = 1;
            if (*(msie + 6) == '.') {
                if (*(msie + 5) == '6') {
                    msie6 = 1;
                    if (ngx_rustscript_substr(msie + 8,
                            (size_t) ((user_agent + ua_len) - (msie + 8)),
                            "SV1", 3) != NULL)
                    {
                        msie6 = 0;
                    }
                } else if (*(msie + 5) < '6') {
                    msie6 = 1;
                }
            }
        }

        if (ngx_rustscript_substr(user_agent, ua_len, "Opera", 5) != NULL) {
            opera = 1;
            msie_f = 0;
            msie6 = 0;
        }

        if (!msie_f && !opera) {
            if (ngx_rustscript_substr(user_agent, ua_len, "Gecko/", 6) != NULL) {
                gecko = 1;
            } else if (ngx_rustscript_substr(user_agent, ua_len, "Chrome/", 7) != NULL) {
                chrome = 1;
                safari = 1;
            } else if (ngx_rustscript_substr(user_agent, ua_len, "Safari/", 7) != NULL
                       && ngx_rustscript_substr(user_agent, ua_len, "Mac OS X", 8) != NULL)
            {
                safari = 1;
            } else if (ngx_rustscript_substr(user_agent, ua_len, "Konqueror", 9) != NULL) {
                konqueror = 1;
            }
        }
    }

    r->headers_in.msie = msie_f;
    r->headers_in.msie6 = msie6;
    r->headers_in.opera = opera;
    r->headers_in.gecko = gecko;
    r->headers_in.chrome = chrome;
    r->headers_in.safari = safari;
    r->headers_in.konqueror = konqueror;
}

static int
ngx_rustscript_ci_substr(const u_char *s, size_t len, const char *needle, size_t nlen)
{
    size_t i, j;

    if (s == NULL || needle == NULL || nlen == 0 || len < nlen) {
        return 0;
    }
    for (i = 0; i + nlen <= len; i++) {
        for (j = 0; j < nlen; j++) {
            u_char a = s[i + j];
            u_char b = (u_char) needle[j];
            if (a >= 'A' && a <= 'Z') {
                a |= 0x20;
            }
            if (b >= 'A' && b <= 'Z') {
                b |= 0x20;
            }
            if (a != b) {
                break;
            }
        }
        if (j == nlen) {
            return 1;
        }
    }
    return 0;
}

void ngx_rustscript_c_headers_in_apply_connection(ngx_http_request_t *r, ngx_table_elt_t *h)
{
    if (r == NULL) {
        return;
    }
    r->headers_in.connection = h;
    r->headers_in.connection_type = 0;
    if (h == NULL || h->value.data == NULL || h->value.len == 0) {
        return;
    }
    if (ngx_rustscript_ci_substr(h->value.data, h->value.len, "close", 5)) {
        r->headers_in.connection_type = NGX_HTTP_CONNECTION_CLOSE;
        return;
    }
    if (ngx_rustscript_ci_substr(h->value.data, h->value.len, "keep-alive", 10)) {
        r->headers_in.connection_type = NGX_HTTP_CONNECTION_KEEP_ALIVE;
    }
}

void ngx_rustscript_c_request_main_count_inc(ngx_http_request_t *r)
{
    if (r == NULL || r->main == NULL) {
        return;
    }
    r->main->count++;
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
