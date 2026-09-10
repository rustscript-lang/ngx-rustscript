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
#include <sys/socket.h>

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

unsigned int ngx_rustscript_c_request_header_only(const ngx_http_request_t *r)
{
    return r != NULL && (r->header_only || r->method == NGX_HTTP_HEAD) ? 1 : 0;
}

unsigned int ngx_rustscript_c_request_is_http10(const ngx_http_request_t *r)
{
    return r != NULL && r->http_version < NGX_HTTP_VERSION_11 ? 1 : 0;
}

ngx_int_t ngx_rustscript_c_send_response_headers(ngx_http_request_t *r)
{
    ngx_int_t rc;

    if (r == NULL) {
        return NGX_ERROR;
    }
    if (r->header_sent) {
        return NGX_OK;
    }
    if (r->headers_out.status == 0) {
        r->headers_out.status = NGX_HTTP_OK;
    }
    rc = ngx_http_set_content_type(r);
    if (rc != NGX_OK) {
        return rc;
    }
    rc = ngx_http_send_header(r);
    if (rc == NGX_OK || rc == NGX_DONE) {
        r->header_sent = 1;
    }
    return rc;
}

ngx_int_t ngx_rustscript_c_output_body(ngx_http_request_t *r, const u_char *data, size_t len,
    unsigned flush, unsigned last)
{
    ngx_buf_t *b;
    ngx_chain_t *cl;
    u_char *p;

    if (r == NULL || r->pool == NULL) {
        return NGX_ERROR;
    }
    b = ngx_palloc(r->pool, sizeof(ngx_buf_t));
    if (b == NULL) {
        return NGX_ERROR;
    }
    ngx_memzero(b, sizeof(ngx_buf_t));
    if (len > 0) {
        if (data == NULL) {
            return NGX_ERROR;
        }
        p = ngx_palloc(r->pool, len);
        if (p == NULL) {
            return NGX_ERROR;
        }
        ngx_memcpy(p, data, len);
        b->pos = p;
        b->last = p + len;
        b->memory = 1;
    } else {
        b->sync = 1;
    }
    b->last_in_chain = 1;
    b->flush = flush ? 1 : 0;
    if (last) {
        if (r->main == r) {
            b->last_buf = 1;
        } else {
            b->sync = 1;
            b->last_buf = 0;
        }
    }
    cl = ngx_palloc(r->pool, sizeof(ngx_chain_t));
    if (cl == NULL) {
        return NGX_ERROR;
    }
    cl->buf = b;
    cl->next = NULL;
    return ngx_http_output_filter(r, cl);
}

ngx_int_t ngx_rustscript_c_output_special(ngx_http_request_t *r, unsigned flush, unsigned last)
{
    ngx_uint_t flags = 0;

    if (r == NULL) {
        return NGX_ERROR;
    }
    if (flush) {
        flags |= NGX_HTTP_FLUSH;
    }
    if (last) {
        flags |= NGX_HTTP_LAST;
    }
    if (flags == 0) {
        return NGX_OK;
    }
    return ngx_http_send_special(r, flags);
}

unsigned ngx_rustscript_stub_output_needs_flush_wait(void) __attribute__((weak));
unsigned ngx_rustscript_stub_skip_event_timers(void) __attribute__((weak));
ngx_int_t ngx_rustscript_stub_flush_continue_result(void) __attribute__((weak));

off_t ngx_rustscript_c_content_length_n(const ngx_http_request_t *r)
{
    return r == NULL ? (off_t) -1 : r->headers_out.content_length_n;
}

static void
ngx_rustscript_flush_timeouts(ngx_http_request_t *r, ngx_msec_t *timeout, size_t *lowat)
{
    ngx_http_core_loc_conf_t *clcf = NULL;
    ngx_http_request_t *main;

    *timeout = 60000;
    *lowat = 0;
    if (r == NULL) {
        return;
    }
    main = r->main != NULL ? r->main : r;
    if (main->loc_conf == NULL) {
        return;
    }
    clcf = ngx_http_get_module_loc_conf(main, ngx_http_core_module);
    if (clcf == NULL) {
        return;
    }
    *timeout = clcf->send_timeout;
    *lowat = clcf->send_lowat;
}

static unsigned
ngx_rustscript_skip_event_timers(void)
{
    return ngx_rustscript_stub_skip_event_timers != NULL
        && ngx_rustscript_stub_skip_event_timers();
}

static void
ngx_rustscript_flush_add_timer(ngx_event_t *wev, ngx_msec_t timeout)
{
    if (wev == NULL) {
        return;
    }
    if (ngx_rustscript_skip_event_timers()) {
        wev->timer_set = 1;
        (void) timeout;
        return;
    }
#ifndef NGX_RUSTSCRIPT_ABI_PROBE
    ngx_add_timer(wev, timeout);
#else
    wev->timer_set = 1;
    (void) timeout;
#endif
}

static void
ngx_rustscript_flush_del_timer(ngx_event_t *wev)
{
    if (wev == NULL || !wev->timer_set || wev->delayed) {
        return;
    }
    if (ngx_rustscript_skip_event_timers()) {
        wev->timer_set = 0;
        return;
    }
#ifndef NGX_RUSTSCRIPT_ABI_PROBE
    ngx_del_timer(wev);
#else
    wev->timer_set = 0;
#endif
}

unsigned int ngx_rustscript_c_connection_needs_flush_wait(ngx_http_request_t *r)
{
    ngx_event_t *wev;

    if (ngx_rustscript_stub_output_needs_flush_wait
        && ngx_rustscript_stub_output_needs_flush_wait())
    {
        return 1;
    }
    if (r == NULL || r->connection == NULL || r->connection->write == NULL) {
        return 0;
    }
    wev = r->connection->write;
    if (wev->delayed) {
        return 1;
    }
    return (r->connection->buffered
            & (NGX_HTTP_LOWLEVEL_BUFFERED | NGX_LOWLEVEL_BUFFERED)) != 0 ? 1 : 0;
}

static void
ngx_rustscript_c_note_log_status(ngx_http_request_t *r, ngx_int_t status)
{
    if (r == NULL) {
        return;
    }
    /* After headers are on the wire, $status uses err_status. Do not rewrite
     * headers_out.status (already sent) and do not call finalize here. */
    if (r->header_sent && r->err_status == 0 && status > 0) {
        r->err_status = (ngx_uint_t) status;
    }
}

static void
ngx_rustscript_log_flush_wait_armed(ngx_http_request_t *r)
{
#ifndef NGX_RUSTSCRIPT_ABI_PROBE
    if (r == NULL || r->connection == NULL || r->connection->log == NULL) {
        return;
    }
    ngx_log_error(NGX_LOG_INFO, r->connection->log, 0, "FLUSH_WAIT_ARMING");
#else
    (void) r;
#endif
}

static unsigned
ngx_rustscript_flush_client_closed(ngx_connection_t *c, ngx_event_t *wev)
{
    if (c == NULL) {
        return 0;
    }
    if (c->error) {
        return 1;
    }
    return wev != NULL && wev->error;
}

static ngx_int_t
ngx_rustscript_flush_continue_status(ngx_http_request_t *r, ngx_int_t rc)
{
    ngx_connection_t *c;
    ngx_event_t *wev;

    if (rc == NGX_AGAIN || rc == NGX_OK || rc == NGX_DONE) {
        return rc;
    }
    c = r != NULL ? r->connection : NULL;
    wev = (c != NULL) ? c->write : NULL;
    if (rc == NGX_HTTP_REQUEST_TIME_OUT) {
        ngx_rustscript_c_note_log_status(r, NGX_HTTP_REQUEST_TIME_OUT);
        return NGX_HTTP_REQUEST_TIME_OUT;
    }
    if (rc == NGX_HTTP_CLIENT_CLOSED_REQUEST
        || ngx_rustscript_flush_client_closed(c, wev))
    {
        ngx_rustscript_c_note_log_status(r, NGX_HTTP_CLIENT_CLOSED_REQUEST);
        return NGX_HTTP_CLIENT_CLOSED_REQUEST;
    }
    return rc;
}

ngx_int_t ngx_rustscript_c_arm_flush_wait(ngx_http_request_t *r, ngx_http_event_handler_pt handler)
{
    ngx_event_t *wev;
    ngx_msec_t timeout;
    size_t lowat;

    if (r == NULL || handler == NULL) {
        return NGX_ERROR;
    }

    /* Request-level write callback only. Leave c->write->handler as the
     * official ngx_http_request_handler so NGINX still owns the event. */
    r->write_event_handler = handler;

    if (ngx_rustscript_stub_output_needs_flush_wait
        && ngx_rustscript_stub_output_needs_flush_wait())
    {
        /* Do not ngx_post_event a ready write: that busy-spins the handler. */
        ngx_rustscript_log_flush_wait_armed(r);
        return NGX_OK;
    }

    if (r->connection == NULL || r->connection->write == NULL) {
        return NGX_ERROR;
    }

    wev = r->connection->write;
    ngx_rustscript_flush_timeouts(r, &timeout, &lowat);
    if (!wev->delayed) {
        ngx_rustscript_flush_add_timer(wev, timeout);
    }
    if (ngx_handle_write_event(wev, lowat) != NGX_OK) {
        return NGX_ERROR;
    }
    ngx_rustscript_log_flush_wait_armed(r);
    return NGX_OK;
}

ngx_int_t ngx_rustscript_c_continue_output(ngx_http_request_t *r)
{
    ngx_connection_t *c;
    ngx_event_t *wev;
    ngx_int_t rc;
    ngx_msec_t timeout;
    size_t lowat;

    if (r == NULL) {
        return NGX_ERROR;
    }
    if (ngx_rustscript_stub_flush_continue_result != NULL) {
        return ngx_rustscript_flush_continue_status(
            r, ngx_rustscript_stub_flush_continue_result());
    }

    c = r->connection;
    if (c == NULL || c->write == NULL) {
        return NGX_ERROR;
    }
    wev = c->write;
    ngx_rustscript_flush_timeouts(r, &timeout, &lowat);

    if (wev->timedout) {
        if (!wev->delayed) {
            c->timedout = 1;
            return ngx_rustscript_flush_continue_status(r, NGX_HTTP_REQUEST_TIME_OUT);
        }
        wev->timedout = 0;
        wev->delayed = 0;
        if (!wev->ready) {
            ngx_rustscript_flush_add_timer(wev, timeout);
            if (ngx_handle_write_event(wev, lowat) != NGX_OK) {
                return NGX_ERROR;
            }
            return NGX_AGAIN;
        }
    }

    if (ngx_rustscript_flush_client_closed(c, wev)) {
        return ngx_rustscript_flush_continue_status(r, NGX_HTTP_CLIENT_CLOSED_REQUEST);
    }

    if (!wev->ready && !wev->timedout) {
        return NGX_AGAIN;
    }

    if (c->buffered & (NGX_HTTP_LOWLEVEL_BUFFERED | NGX_LOWLEVEL_BUFFERED)) {
        rc = ngx_http_output_filter(r, NULL);
        if (rc == NGX_ERROR || rc > NGX_OK) {
            return ngx_rustscript_flush_continue_status(r, rc);
        }
        if (c->buffered & (NGX_HTTP_LOWLEVEL_BUFFERED | NGX_LOWLEVEL_BUFFERED)) {
            if (!wev->delayed) {
                ngx_rustscript_flush_add_timer(wev, timeout);
            }
            if (ngx_handle_write_event(wev, lowat) != NGX_OK) {
                return NGX_ERROR;
            }
            return NGX_AGAIN;
        }
    }

    ngx_rustscript_flush_del_timer(wev);
    return NGX_OK;
}

void ngx_rustscript_c_disarm_flush_wait(ngx_http_request_t *r)
{
    /* Caller restores r->write_event_handler. Never replace core wev handlers. */
    if (r == NULL || r->connection == NULL || r->connection->write == NULL) {
        return;
    }
    ngx_rustscript_flush_del_timer(r->connection->write);
}

void ngx_rustscript_c_note_client_closed_log_status(ngx_http_request_t *r)
{
    ngx_rustscript_c_note_log_status(r, NGX_HTTP_CLIENT_CLOSED_REQUEST);
}

void ngx_rustscript_c_test_reading_and_note_client_closed(ngx_http_request_t *r)
{
    ngx_connection_t *c;
    char buf[1];
    ssize_t n;
    ngx_err_t err;

    if (r == NULL) {
        return;
    }
    c = r->connection;
    if (r->header_sent && r->err_status == 0 && c != NULL
        && c->fd != (ngx_socket_t) -1)
    {
        n = recv(c->fd, buf, 1, MSG_PEEK);
        err = ngx_socket_errno;
        if (n == 0 || c->error
            || (n == -1 && err != NGX_EAGAIN && err != NGX_EINTR))
        {
            r->err_status = NGX_HTTP_CLIENT_CLOSED_REQUEST;
        }
    }
#ifndef NGX_RUSTSCRIPT_ABI_PROBE
    ngx_http_test_reading(r);
#endif
}

void ngx_rustscript_c_set_content_length(ngx_http_request_t *r, off_t n)
{
    if (r == NULL) {
        return;
    }
    r->headers_out.content_length_n = n;
}

unsigned int ngx_rustscript_c_request_main_count(const ngx_http_request_t *r)
{
    if (r == NULL || r->main == NULL) {
        return 0;
    }
    return r->main->count;
}

void ngx_rustscript_c_request_main_count_dec(ngx_http_request_t *r)
{
    if (r == NULL || r->main == NULL) {
        return;
    }
    if (r->main->count > 0) {
        r->main->count--;
    }
}

ngx_int_t ngx_rustscript_c_read_client_request_body(
    ngx_http_request_t *r,
    void (*handler)(ngx_http_request_t *r)
)
{
    ngx_int_t rc;

    if (r == NULL) {
        return NGX_ERROR;
    }
#ifndef NGX_RUSTSCRIPT_ABI_PROBE
    rc = ngx_http_read_client_request_body(r, handler);
    /* Core maps premature EOF to 400 + c->error. Confirmed client close is 499. */
    if (rc == NGX_HTTP_BAD_REQUEST && r->connection != NULL && r->connection->error) {
        return NGX_HTTP_CLIENT_CLOSED_REQUEST;
    }
    return rc;
#else
    (void)handler;
    return NGX_ERROR;
#endif
}

ngx_int_t ngx_rustscript_c_discard_request_body(ngx_http_request_t *r)
{
    if (r == NULL) {
        return NGX_ERROR;
    }
#ifndef NGX_RUSTSCRIPT_ABI_PROBE
    return ngx_http_discard_request_body(r);
#else
    return NGX_ERROR;
#endif
}

ngx_http_event_handler_pt
ngx_rustscript_c_get_read_event_handler(ngx_http_request_t *r)
{
    if (r == NULL) {
        return NULL;
    }
    return r->read_event_handler;
}

void ngx_rustscript_c_set_read_event_handler(ngx_http_request_t *r,
                                            ngx_http_event_handler_pt handler)
{
    if (r == NULL) {
        return;
    }
    r->read_event_handler = handler;
}

unsigned int ngx_rustscript_c_connection_error(const ngx_http_request_t *r)
{
    if (r == NULL || r->connection == NULL) {
        return 0;
    }
    return r->connection->error != 0;
}

unsigned int ngx_rustscript_c_request_terminated(const ngx_http_request_t *r)
{
    if (r == NULL || r->main == NULL || r->connection == NULL) {
        return 1;
    }
    return 0;
}

void ngx_rustscript_c_block_reading(ngx_http_request_t *r)
{
    if (r == NULL) {
        return;
    }
#ifndef NGX_RUSTSCRIPT_ABI_PROBE
    r->read_event_handler = ngx_http_block_reading;
#endif
}

#ifndef NGX_RUSTSCRIPT_ABI_PROBE
void ngx_rustscript_continue_body_read(ngx_http_request_t *r);
void ngx_rustscript_on_body_wait_closed(ngx_http_request_t *r);

static void ngx_rustscript_c_body_wait_read_handler(ngx_http_request_t *r)
{
    ngx_connection_t *c;
    ngx_event_t *rev;
    char buf[1];
    ssize_t n;
    ngx_err_t err;

    if (r == NULL) {
        return;
    }
    c = r->connection;
    rev = (c != NULL) ? c->read : NULL;

    /* Timeout must reach NGINX's body handler so the status stays 408. */
    if (rev != NULL && rev->timedout) {
        ngx_rustscript_continue_body_read(r);
        return;
    }

    if (c != NULL && (c->error || (rev != NULL && (rev->error || rev->pending_eof)))) {
        ngx_rustscript_on_body_wait_closed(r);
        return;
    }

    if (c != NULL && c->fd != (ngx_socket_t) -1) {
        n = recv(c->fd, buf, 1, MSG_PEEK);
        err = ngx_socket_errno;
        if (n == 0 || (n == -1 && err != NGX_EAGAIN && err != NGX_EINTR)) {
            ngx_rustscript_on_body_wait_closed(r);
            return;
        }
    }

    ngx_rustscript_continue_body_read(r);
}

ngx_int_t ngx_rustscript_c_arm_body_wait(ngx_http_request_t *r)
{
    ngx_event_t *rev;

    if (r == NULL || r->connection == NULL || r->connection->read == NULL) {
        return NGX_ERROR;
    }
    r->read_event_handler = ngx_rustscript_c_body_wait_read_handler;
    rev = r->connection->read;
    if (rev->ready) {
        ngx_post_event(rev, &ngx_posted_events);
        return NGX_OK;
    }
    if (ngx_handle_read_event(rev, 0) != NGX_OK) {
        return NGX_ERROR;
    }
    return NGX_OK;
}

void ngx_rustscript_c_finalize_client_closed(ngx_http_request_t *r)
{
    if (r == NULL) {
        return;
    }
    r->lingering_close = 1;
    ngx_rustscript_c_note_log_status(r, NGX_HTTP_CLIENT_CLOSED_REQUEST);
    ngx_http_finalize_request(r, NGX_HTTP_CLIENT_CLOSED_REQUEST);
}
#else
ngx_int_t ngx_rustscript_c_arm_body_wait(ngx_http_request_t *r)
{
    (void)r;
    return NGX_ERROR;
}

void ngx_rustscript_c_finalize_client_closed(ngx_http_request_t *r)
{
    (void)r;
}
#endif

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
