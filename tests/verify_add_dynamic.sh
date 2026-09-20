#!/usr/bin/env bash
# Full NGINX --add-dynamic-module configure + parallel make + nginx -t.
#
# Cargo tests cannot run this in-process: NGINX make invokes cargo rustc and would
# deadlock on the same package lock. Invoke this script from verification after
# `cargo build --lib` / `cargo test`.
set -euo pipefail

CRATE_ROOT="${CRATE_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
TMP_ROOT="${NGX_RUSTSCRIPT_TASK_TMP:-${TMPDIR:-/tmp}/ngx-rustscript-add-dynamic}"
NGINX_SOURCE_DIR="${NGINX_SOURCE_DIR:-$TMP_ROOT/nginx-1.31.4}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$CRATE_ROOT/target}"
SERVER_VARIANT="${NGX_RUSTSCRIPT_SERVER_VARIANT:-nginx}"

case "$SERVER_VARIANT" in
  nginx)
    EXPECTED_BANNER="nginx/1.31.4"
    ;;
  openresty)
    EXPECTED_BANNER="openresty/1.31.4.1"
    ;;
  *)
    echo "unsupported NGX_RUSTSCRIPT_SERVER_VARIANT: $SERVER_VARIANT" >&2
    exit 1
    ;;
esac

if [[ ! -x "$NGINX_SOURCE_DIR/configure" ]]; then
  echo "NGINX_SOURCE_DIR is not a configured 1.31.4 tree: $NGINX_SOURCE_DIR" >&2
  exit 1
fi

STAMP="$(date +%Y%m%d%H%M%S)-$$"
PREFIX="$TMP_ROOT/add-dynamic-verify-prefix-$STAMP"
BUILDDIR="$TMP_ROOT/add-dynamic-verify-objs-$STAMP"
CONF_PREFIX="$TMP_ROOT/add-dynamic-verify-run-$STAMP"
MODULE_NAME=ngx_http_rustscript_module.so
MAKEFILE_BACKUP="$TMP_ROOT/add-dynamic-verify-Makefile-$STAMP"

mkdir -p "$PREFIX" "$BUILDDIR" "$CONF_PREFIX/conf" "$CONF_PREFIX/logs" "$CONF_PREFIX/html"
cp "$NGINX_SOURCE_DIR/Makefile" "$MAKEFILE_BACKUP"

cleanup() {
  cp "$MAKEFILE_BACKUP" "$NGINX_SOURCE_DIR/Makefile"
  rm -f "$MAKEFILE_BACKUP"
  rm -rf "$PREFIX" "$BUILDDIR" "$CONF_PREFIX"
}
trap cleanup EXIT

unset CFLAGS CPPFLAGS BINDGEN_EXTRA_CLANG_ARGS
export CARGO_NET_GIT_FETCH_WITH_CLI=true

(
  cd "$NGINX_SOURCE_DIR"
  ./configure \
    --prefix="$PREFIX" \
    --with-compat \
    --with-http_ssl_module \
    --builddir="$BUILDDIR" \
    --add-dynamic-module="$CRATE_ROOT"
  make -j"$(nproc)" -f "$BUILDDIR/Makefile"
)

NGINX_BIN="$BUILDDIR/nginx"
MODULE_SO="$BUILDDIR/$MODULE_NAME"
if [[ ! -x "$NGINX_BIN" ]]; then
  echo "add-dynamic make did not emit $NGINX_BIN" >&2
  exit 1
fi
if [[ ! -f "$MODULE_SO" ]]; then
  echo "add-dynamic make did not emit $MODULE_SO" >&2
  exit 1
fi

BANNER="$("$NGINX_BIN" -v 2>&1 || true)"
if [[ "$BANNER" != *"$EXPECTED_BANNER"* ]]; then
  echo "emitted server does not match $EXPECTED_BANNER: $BANNER" >&2
  exit 1
fi
REPORT="$("$NGINX_BIN" -V 2>&1 || true)"
if [[ "$REPORT" != *--with-compat* || "$REPORT" != *--with-http_ssl_module* ]]; then
  echo "emitted nginx is missing required configure flags: $REPORT" >&2
  exit 1
fi

NM_OUT="$(nm -D --defined-only --format=posix "$MODULE_SO" || true)"
for stub in ngx_conf_full_name ngx_conf_log_error ngx_array_push ngx_log_error_core \
  ngx_pool_cleanup_add ngx_pool_cleanup_file ngx_pool_delete_file ngx_palloc ngx_http_core_module ngx_http_core_run_phases ngx_get_connection \
  ngx_free_connection ngx_close_connection ngx_event_actions ngx_event_flags ngx_process \
  ngx_http_test_reading ngx_http_block_reading ngx_handle_read_event ngx_posted_events \
  ngx_pid ngx_worker ngx_exiting ngx_cycle ngx_core_module ngx_processes ngx_http_get_variable \
  ngx_list_push ngx_pnalloc ngx_hash_find ngx_hash_strlow ngx_http_parse_time ngx_parse_http_time ngx_http_set_exten \
  ngx_http_send_header ngx_http_send_special ngx_http_output_filter ngx_http_set_content_type \
  ngx_handle_write_event ngx_http_read_client_request_body ngx_http_discard_request_body \
  ngx_http_finalize_request \
  ngx_rustscript_test_request_new ngx_rustscript_test_request_free ngx_rustscript_test_request_sizeof \
  ngx_rustscript_test_request_set_internal ngx_rustscript_test_request_set_filter_finalize \
  ngx_rustscript_test_request_set_error_page ngx_rustscript_test_request_set_header_sent \
  ngx_rustscript_test_request_set_reading_body ngx_rustscript_test_request_set_discard_body \
  ngx_rustscript_test_request_set_header_only \
  ngx_rustscript_stub_alloc_fail_after ngx_rustscript_stub_alloc_reset \
  ngx_rustscript_test_headers_in_msie ngx_rustscript_test_install_variable; do
  if awk -v symbol="$stub" '$1 == symbol { found = 1 } END { exit !found }' <<< "$NM_OUT"; then
    echo "add-dynamic module must not define test NGINX stub $stub" >&2
    exit 1
  fi
done

for shim in ngx_rustscript_request_is_internal ngx_rustscript_request_headers_sent \
  ngx_rustscript_request_reading_body ngx_rustscript_request_discard_body; do
  if ! awk -v symbol="$shim" '$1 == symbol { found = 1 } END { exit !found }' <<< "$NM_OUT"; then
    echo "add-dynamic module must define request shim $shim" >&2
    exit 1
  fi
done

MODULE_ESCAPED="${MODULE_SO//\\/\\\\}"
MODULE_ESCAPED="${MODULE_ESCAPED//\"/\\\"}"
PREFIX_ESCAPED="${CONF_PREFIX//\\/\\\\}"
PREFIX_ESCAPED="${PREFIX_ESCAPED//\"/\\\"}"
cat > "$CONF_PREFIX/conf/nginx.conf" <<EOF
worker_processes 1;
error_log stderr emerg;
pid "$PREFIX_ESCAPED/logs/nginx.pid";
daemon off;
master_process off;
load_module "$MODULE_ESCAPED";
events {
    worker_connections 32;
}
http {
    access_log off;
    server {
        listen 127.0.0.1:18080;
        server_name localhost;
        location / {
            return 200 "ok\\n";
        }
    }
}
EOF

if ! "$NGINX_BIN" -t -p "$CONF_PREFIX" -c conf/nginx.conf; then
  echo "nginx -t failed for add-dynamic module" >&2
  exit 1
fi

echo "add-dynamic verify ok: $MODULE_SO"
