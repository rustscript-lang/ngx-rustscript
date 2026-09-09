/* Test-only NGINX request bitfield ABI compiled against official headers. */

#include <nginx.h>
#include <ngx_config.h>
#include <ngx_core.h>
#include <ngx_http.h>

#include <stdlib.h>
#include <string.h>

extern void *(*ngx_rustscript_stub_hash_find_hook)(
    void *hash, ngx_uint_t key, u_char *name, size_t len);

ngx_http_request_t *ngx_rustscript_test_request_new(void)
{
    return calloc(1, sizeof(ngx_http_request_t));
}

void ngx_rustscript_test_request_free(ngx_http_request_t *r)
{
    free(r);
}

size_t ngx_rustscript_test_request_sizeof(void)
{
    return sizeof(ngx_http_request_t);
}

void ngx_rustscript_test_request_set_internal(ngx_http_request_t *r, unsigned v)
{
    if (r != NULL) {
        r->internal = v ? 1 : 0;
    }
}

void ngx_rustscript_test_request_set_filter_finalize(ngx_http_request_t *r, unsigned v)
{
    if (r != NULL) {
        r->filter_finalize = v ? 1 : 0;
    }
}

void ngx_rustscript_test_request_set_error_page(ngx_http_request_t *r, unsigned v)
{
    if (r != NULL) {
        r->error_page = v ? 1 : 0;
    }
}

void ngx_rustscript_test_request_set_header_sent(ngx_http_request_t *r, unsigned v)
{
    if (r != NULL) {
        r->header_sent = v ? 1 : 0;
    }
}

void ngx_rustscript_test_request_set_reading_body(ngx_http_request_t *r, unsigned v)
{
    if (r != NULL) {
        r->reading_body = v ? 1 : 0;
    }
}

void ngx_rustscript_test_request_set_discard_body(ngx_http_request_t *r, unsigned v)
{
    if (r != NULL) {
        r->discard_body = v ? 1 : 0;
    }
}

unsigned ngx_rustscript_test_request_get_internal(ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->internal;
}

unsigned ngx_rustscript_test_request_get_valid_unparsed_uri(ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->valid_unparsed_uri;
}

unsigned ngx_rustscript_test_request_get_valid_location(ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->valid_location;
}

unsigned ngx_rustscript_test_request_get_uri_changed(ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->uri_changed;
}

void ngx_rustscript_test_request_set_valid_unparsed_uri(ngx_http_request_t *r, unsigned v)
{
    if (r != NULL) {
        r->valid_unparsed_uri = v ? 1 : 0;
    }
}

void ngx_rustscript_test_request_set_valid_location(ngx_http_request_t *r, unsigned v)
{
    if (r != NULL) {
        r->valid_location = v ? 1 : 0;
    }
}

void ngx_rustscript_test_request_set_uri_changed(ngx_http_request_t *r, unsigned v)
{
    if (r != NULL) {
        r->uri_changed = v ? 1 : 0;
    }
}

unsigned ngx_rustscript_test_var_not_found(const ngx_http_variable_value_t *vv)
{
    return vv == NULL ? 1 : vv->not_found;
}

unsigned ngx_rustscript_test_var_valid(const ngx_http_variable_value_t *vv)
{
    return vv == NULL ? 0 : vv->valid;
}

unsigned ngx_rustscript_test_var_no_cacheable(const ngx_http_variable_value_t *vv)
{
    return vv == NULL ? 0 : vv->no_cacheable;
}

unsigned ngx_rustscript_test_var_len(const ngx_http_variable_value_t *vv)
{
    return vv == NULL ? 0 : vv->len;
}

u_char *ngx_rustscript_test_var_data(const ngx_http_variable_value_t *vv)
{
    return vv == NULL ? NULL : vv->data;
}

void ngx_rustscript_test_request_set_err_status(ngx_http_request_t *r, ngx_uint_t status)
{
    if (r != NULL) {
        r->err_status = status;
    }
}

ngx_uint_t ngx_rustscript_test_request_err_status(const ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->err_status;
}

void ngx_rustscript_test_request_set_http_version(ngx_http_request_t *r, ngx_uint_t version)
{
    if (r != NULL) {
        r->http_version = version;
    }
}

ngx_uint_t ngx_rustscript_test_request_http_version(const ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->http_version;
}

unsigned ngx_rustscript_test_headers_in_msie(const ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->headers_in.msie;
}

unsigned ngx_rustscript_test_headers_in_msie6(const ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->headers_in.msie6;
}

unsigned ngx_rustscript_test_headers_in_opera(const ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->headers_in.opera;
}

unsigned ngx_rustscript_test_headers_in_gecko(const ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->headers_in.gecko;
}

unsigned ngx_rustscript_test_headers_in_chrome(const ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->headers_in.chrome;
}

unsigned ngx_rustscript_test_headers_in_safari(const ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->headers_in.safari;
}

unsigned ngx_rustscript_test_headers_in_konqueror(const ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->headers_in.konqueror;
}

unsigned ngx_rustscript_test_headers_in_connection_type(const ngx_http_request_t *r)
{
    return r == NULL ? 0 : (unsigned) r->headers_in.connection_type;
}

ngx_table_elt_t *ngx_rustscript_test_headers_in_connection(const ngx_http_request_t *r)
{
    return r == NULL ? NULL : r->headers_in.connection;
}

ngx_table_elt_t *ngx_rustscript_test_headers_in_host(const ngx_http_request_t *r)
{
    return r == NULL ? NULL : r->headers_in.host;
}

ngx_str_t ngx_rustscript_test_headers_in_server(const ngx_http_request_t *r)
{
    ngx_str_t empty = ngx_null_string;
    return r == NULL ? empty : r->headers_in.server;
}

size_t ngx_rustscript_test_headers_in_server_len(const ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->headers_in.server.len;
}

u_char *ngx_rustscript_test_headers_in_server_data(const ngx_http_request_t *r)
{
    return r == NULL ? NULL : r->headers_in.server.data;
}

off_t ngx_rustscript_test_headers_in_content_length_n(const ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->headers_in.content_length_n;
}

ngx_table_elt_t *ngx_rustscript_test_headers_in_content_length(const ngx_http_request_t *r)
{
    return r == NULL ? NULL : r->headers_in.content_length;
}

ngx_table_elt_t *ngx_rustscript_test_headers_in_user_agent(const ngx_http_request_t *r)
{
    return r == NULL ? NULL : r->headers_in.user_agent;
}

ngx_table_elt_t *ngx_rustscript_test_headers_out_location(const ngx_http_request_t *r)
{
    return r == NULL ? NULL : r->headers_out.location;
}

ngx_uint_t ngx_rustscript_test_headers_out_status(const ngx_http_request_t *r)
{
    return r == NULL ? 0 : r->headers_out.status;
}

/* ---- fail-after-N allocator (default: always fail, matching prior stubs) ---- */

static int stub_alloc_fail_after = -1;
static int stub_alloc_count;

void ngx_rustscript_stub_alloc_reset(void)
{
    stub_alloc_fail_after = -1;
    stub_alloc_count = 0;
}

void ngx_rustscript_stub_alloc_fail_after(int n)
{
    stub_alloc_fail_after = n;
    stub_alloc_count = 0;
}

int ngx_rustscript_stub_alloc_count(void)
{
    return stub_alloc_count;
}

static int stub_alloc_should_fail(void)
{
    if (stub_alloc_fail_after < 0) {
        return 1;
    }
    if (stub_alloc_count >= stub_alloc_fail_after) {
        return 1;
    }
    stub_alloc_count++;
    return 0;
}

void *ngx_palloc(ngx_pool_t *pool, size_t size)
{
    (void) pool;
    if (size == 0 || stub_alloc_should_fail()) {
        return NULL;
    }
    return calloc(1, size);
}

void *ngx_pnalloc(ngx_pool_t *pool, size_t size)
{
    return ngx_palloc(pool, size);
}

void *ngx_list_push(ngx_list_t *l)
{
    ngx_list_part_t *last;
    void            *elt;

    if (l == NULL) {
        return NULL;
    }

    last = l->last;
    if (last == NULL) {
        return NULL;
    }

    if (last->nelts >= l->nalloc) {
        last = ngx_palloc(l->pool, sizeof(ngx_list_part_t));
        if (last == NULL) {
            return NULL;
        }

        last->elts = ngx_palloc(l->pool, l->nalloc * l->size);
        if (last->elts == NULL) {
            return NULL;
        }

        last->nelts = 0;
        last->next = NULL;

        l->last->next = last;
        l->last = last;
    }

    if (last->elts == NULL) {
        return NULL;
    }

    elt = (char *) last->elts + l->size * last->nelts;
    last->nelts++;

    return elt;
}

/* ---- send_header / send_special recording ---- */

static ngx_int_t stub_send_header_rc;
static ngx_int_t stub_send_special_rc;
static int stub_send_header_calls;
static int stub_send_special_calls;
static ngx_uint_t stub_send_special_last_flags;

enum { STUB_OUTPUT_CAP = 65536 };

static ngx_int_t stub_output_rc;
static int stub_output_calls;
static unsigned char stub_output_body[STUB_OUTPUT_CAP];
static size_t stub_output_len;
static unsigned stub_output_last;
static unsigned stub_output_flush;
static unsigned stub_output_last_in_chain;
static unsigned stub_output_sync;
static unsigned stub_needs_flush_wait;
static ngx_int_t stub_flush_continue_rc;

void ngx_rustscript_stub_output_reset(void)
{
    stub_output_rc = NGX_OK;
    stub_output_calls = 0;
    stub_output_len = 0;
    stub_output_last = 0;
    stub_output_flush = 0;
    stub_output_last_in_chain = 0;
    stub_output_sync = 0;
    stub_needs_flush_wait = 0;
    stub_flush_continue_rc = NGX_OK;
    memset(stub_output_body, 0, sizeof(stub_output_body));
}

void ngx_rustscript_stub_output_set_rc(ngx_int_t rc)
{
    stub_output_rc = rc;
}

void ngx_rustscript_stub_output_set_needs_flush_wait(unsigned v)
{
    stub_needs_flush_wait = v ? 1 : 0;
}

int ngx_rustscript_stub_output_calls(void)
{
    return stub_output_calls;
}

size_t ngx_rustscript_stub_output_len(void)
{
    return stub_output_len;
}

const unsigned char *ngx_rustscript_stub_output_body(void)
{
    return stub_output_body;
}

unsigned ngx_rustscript_stub_output_saw_last(void)
{
    return stub_output_last;
}

unsigned ngx_rustscript_stub_output_saw_flush(void)
{
    return stub_output_flush;
}

unsigned ngx_rustscript_stub_output_saw_last_in_chain(void)
{
    return stub_output_last_in_chain;
}

unsigned ngx_rustscript_stub_output_needs_flush_wait(void)
{
    return stub_needs_flush_wait;
}

unsigned ngx_rustscript_stub_skip_event_timers(void)
{
    return 1;
}

ngx_rbtree_t ngx_event_timer_rbtree;
volatile ngx_msec_t ngx_current_msec;

void ngx_rustscript_stub_set_flush_continue(ngx_int_t rc)
{
    stub_flush_continue_rc = rc;
}

ngx_int_t ngx_rustscript_stub_flush_continue_result(void)
{
    return stub_flush_continue_rc;
}

void ngx_rustscript_test_request_set_main(ngx_http_request_t *r, ngx_http_request_t *main)
{
    if (r != NULL) {
        r->main = main;
    }
}

void ngx_rustscript_test_request_set_content_length_n(ngx_http_request_t *r, off_t n)
{
    if (r != NULL) {
        r->headers_out.content_length_n = n;
    }
}

off_t ngx_rustscript_test_request_content_length_n(ngx_http_request_t *r)
{
    return r == NULL ? (off_t) -1 : r->headers_out.content_length_n;
}

void ngx_rustscript_test_request_set_header_only(ngx_http_request_t *r, unsigned v)
{
    if (r != NULL) {
        r->header_only = v ? 1 : 0;
    }
}

void ngx_rustscript_stub_send_reset(void)
{
    stub_send_header_rc = NGX_OK;
    stub_send_special_rc = NGX_OK;
    stub_send_header_calls = 0;
    stub_send_special_calls = 0;
    stub_send_special_last_flags = 0;
    ngx_rustscript_stub_output_reset();
}

void ngx_rustscript_stub_send_header_returns(ngx_int_t rc)
{
    stub_send_header_rc = rc;
}

void ngx_rustscript_stub_send_special_returns(ngx_int_t rc)
{
    stub_send_special_rc = rc;
}

int ngx_rustscript_stub_send_header_calls(void)
{
    return stub_send_header_calls;
}

int ngx_rustscript_stub_send_special_calls(void)
{
    return stub_send_special_calls;
}

ngx_uint_t ngx_rustscript_stub_send_special_last_flags(void)
{
    return stub_send_special_last_flags;
}

ngx_int_t ngx_http_send_header(ngx_http_request_t *r)
{
    stub_send_header_calls++;
    (void) r;
    return stub_send_header_rc;
}

ngx_int_t ngx_http_send_special(ngx_http_request_t *r, ngx_uint_t flags)
{
    stub_send_special_calls++;
    stub_send_special_last_flags = flags;
    if (flags & NGX_HTTP_LAST) {
        stub_output_last = 1;
        stub_output_last_in_chain = 1;
    }
    if (flags & NGX_HTTP_FLUSH) {
        stub_output_flush = 1;
    }
    (void) r;
    return stub_send_special_rc;
}

static void stub_record_chain(ngx_chain_t *in)
{
    for (; in != NULL; in = in->next) {
        ngx_buf_t *b = in->buf;
        if (b == NULL) {
            continue;
        }
        if (b->last_buf) {
            stub_output_last = 1;
        }
        if (b->last_in_chain) {
            stub_output_last_in_chain = 1;
        }
        if (b->flush) {
            stub_output_flush = 1;
        }
        if (b->sync) {
            stub_output_sync = 1;
        }
        if (b->pos != NULL && b->last != NULL && b->last > b->pos) {
            size_t n = (size_t) (b->last - b->pos);
            if (stub_output_len + n > STUB_OUTPUT_CAP) {
                n = STUB_OUTPUT_CAP - stub_output_len;
            }
            if (n > 0) {
                memcpy(stub_output_body + stub_output_len, b->pos, n);
                stub_output_len += n;
            }
        }
    }
}

ngx_int_t ngx_http_output_filter(ngx_http_request_t *r, ngx_chain_t *in)
{
    stub_output_calls++;
    stub_record_chain(in);
    (void) r;
    return stub_output_rc;
}

ngx_int_t ngx_http_set_content_type(ngx_http_request_t *r)
{
    static u_char plain[] = "text/plain";
    if (r == NULL) {
        return NGX_ERROR;
    }
    if (r->headers_out.content_type.len == 0) {
        r->headers_out.content_type.data = plain;
        r->headers_out.content_type.len = sizeof(plain) - 1;
    }
    return NGX_OK;
}

ngx_int_t ngx_handle_write_event(ngx_event_t *wev, size_t lowat)
{
    (void) wev;
    (void) lowat;
    return NGX_OK;
}

off_t ngx_atoof(u_char *line, size_t n)
{
    off_t  value, cutoff, cutlim;

    if (n == 0) {
        return NGX_ERROR;
    }

    cutoff = NGX_MAX_OFF_T_VALUE / 10;
    cutlim = NGX_MAX_OFF_T_VALUE % 10;

    for (value = 0; n--; line++) {
        if (*line < '0' || *line > '9') {
            return NGX_ERROR;
        }

        if (value >= cutoff && (value > cutoff || *line - '0' > cutlim)) {
            return NGX_ERROR;
        }

        value = value * 10 + (*line - '0');
    }

    return value;
}

u_char *
ngx_strstrn(u_char *s1, char *s2, size_t n)
{
    u_char  c1, c2;

    c2 = *(u_char *) s2++;

    do {
        do {
            c1 = *s1++;

            if (c1 == 0) {
                return NULL;
            }

        } while (c1 != c2);

    } while (ngx_strncmp(s1, (u_char *) s2, n) != 0);

    return --s1;
}

ngx_int_t
ngx_strncasecmp(u_char *s1, u_char *s2, size_t n)
{
    ngx_uint_t  c1, c2;

    while (n) {
        c1 = (ngx_uint_t) *s1++;
        c2 = (ngx_uint_t) *s2++;

        c1  = (c1 >= 'A' && c1 <= 'Z') ? (c1 | 0x20) : c1;
        c2  = (c2 >= 'A' && c2 <= 'Z') ? (c2 | 0x20) : c2;

        if (c1 == c2) {
            if (c1) {
                n--;
                continue;
            }
            return 0;
        }

        return c1 > c2 ? 1 : -1;
    }

    return 0;
}

void
ngx_strlow(u_char *dst, u_char *src, size_t n)
{
    while (n) {
        *dst = ngx_tolower(*src);
        dst++;
        src++;
        n--;
    }
}

u_char *
ngx_strcasestrn(u_char *s1, char *s2, size_t n)
{
    ngx_uint_t  c1, c2;

    c2 = (ngx_uint_t) *s2++;
    c2  = (c2 >= 'A' && c2 <= 'Z') ? (c2 | 0x20) : c2;

    do {
        do {
            c1 = (ngx_uint_t) *s1++;

            if (c1 == 0) {
                return NULL;
            }

            c1  = (c1 >= 'A' && c1 <= 'Z') ? (c1 | 0x20) : c1;

        } while (c1 != c2);

    } while (ngx_strncasecmp(s1, (u_char *) s2, n) != 0);

    return --s1;
}

/* ---- variable / $host cache test fixtures ---- */

static ngx_http_core_main_conf_t test_cmcf;
static ngx_http_variable_t test_vars[8];
static ngx_http_variable_value_t test_values[8];
static void *test_main_conf[8];
static u_char test_var_names[8][64];
static int test_var_count;
static int test_set_handler_calls;
static ngx_http_variable_value_t *test_last_set_vv;
static uintptr_t test_last_set_data;
static ngx_http_request_t *test_last_set_r;

static void
test_recorded_set_handler(ngx_http_request_t *r, ngx_http_variable_value_t *v,
    uintptr_t data)
{
    test_set_handler_calls++;
    test_last_set_vv = v;
    test_last_set_data = data;
    test_last_set_r = r;
}

static void *
test_hash_find(void *hash, ngx_uint_t key, u_char *name, size_t len)
{
    int i;

    (void) hash;
    (void) key;
    if (name == NULL) {
        return NULL;
    }
    for (i = 0; i < test_var_count; i++) {
        if (test_vars[i].name.len == len
            && memcmp(test_vars[i].name.data, name, len) == 0)
        {
            return &test_vars[i];
        }
    }
    return NULL;
}

void ngx_rustscript_stub_var_reset(void)
{
    memset(&test_cmcf, 0, sizeof(test_cmcf));
    memset(test_vars, 0, sizeof(test_vars));
    memset(test_values, 0, sizeof(test_values));
    memset(test_main_conf, 0, sizeof(test_main_conf));
    memset(test_var_names, 0, sizeof(test_var_names));
    test_var_count = 0;
    test_set_handler_calls = 0;
    test_last_set_vv = NULL;
    test_last_set_data = 0;
    test_last_set_r = NULL;
    ngx_rustscript_stub_hash_find_hook = test_hash_find;
}

int ngx_rustscript_test_install_variable(
    ngx_http_request_t *r,
    const char *name,
    int indexed,
    int changeable,
    int has_handler,
    ngx_uint_t index)
{
    ngx_http_variable_t *v;
    size_t nlen;

    if (r == NULL || name == NULL || test_var_count >= 8) {
        return -1;
    }

    nlen = strlen(name);
    if (nlen >= sizeof(test_var_names[0])) {
        return -1;
    }

    memcpy(test_var_names[test_var_count], name, nlen);
    v = &test_vars[test_var_count];
    v->name.data = test_var_names[test_var_count];
    v->name.len = nlen;
    v->index = index;
    v->data = (uintptr_t) (0x1000 + test_var_count);
    if (changeable) {
        v->flags |= NGX_HTTP_VAR_CHANGEABLE;
    }
    if (indexed) {
        v->flags |= NGX_HTTP_VAR_INDEXED;
    }
    if (has_handler) {
        v->set_handler = test_recorded_set_handler;
    }

    test_var_count++;
    test_cmcf.variables.nelts = (ngx_uint_t) test_var_count;
    test_cmcf.variables.elts = test_vars;

    test_main_conf[0] = &test_cmcf;
    r->main_conf = test_main_conf;
    r->variables = test_values;

    ngx_rustscript_stub_hash_find_hook = test_hash_find;
    return 0;
}

void ngx_rustscript_test_seed_indexed_var(
    ngx_uint_t index, u_char *data, unsigned len, unsigned valid)
{
    if (index >= 8) {
        return;
    }
    test_values[index].data = data;
    test_values[index].len = len;
    test_values[index].valid = valid ? 1 : 0;
    test_values[index].not_found = 0;
    test_values[index].no_cacheable = 0;
}

int ngx_rustscript_test_set_handler_calls(void)
{
    return test_set_handler_calls;
}

ngx_http_variable_value_t *ngx_rustscript_test_last_set_vv(void)
{
    return test_last_set_vv;
}

uintptr_t ngx_rustscript_test_last_set_data(void)
{
    return test_last_set_data;
}

ngx_http_variable_value_t *ngx_rustscript_test_indexed_var(ngx_uint_t index)
{
    if (index >= 8) {
        return NULL;
    }
    return &test_values[index];
}

unsigned ngx_rustscript_test_indexed_var_valid(ngx_uint_t index)
{
    if (index >= 8) {
        return 0;
    }
    return test_values[index].valid;
}

unsigned ngx_rustscript_test_indexed_var_not_found(ngx_uint_t index)
{
    if (index >= 8) {
        return 1;
    }
    return test_values[index].not_found;
}

ngx_uint_t ngx_rustscript_test_cmcf_variables_nelts(void)
{
    return test_cmcf.variables.nelts;
}
