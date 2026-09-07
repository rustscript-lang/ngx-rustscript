#!/usr/bin/env bash
# Full NGINX --add-dynamic-module configure + parallel make + identity + nginx -t.
#
# Cargo tests cannot run this in-process: NGINX make invokes cargo rustc and would
# deadlock on the same package lock. Invoke this script from verification after
# `cargo build --lib` / `cargo test`.
set -euo pipefail

CRATE_ROOT="${CRATE_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
TMP_ROOT="${NGX_RUSTSCRIPT_TASK_TMP:-/mnt/TEMP/workspace/ngx-rustscript/tmp/ngx-rustscript-task1-api-build-4286cfb4}"
NGINX_SOURCE_DIR="${NGINX_SOURCE_DIR:-$TMP_ROOT/nginx-1.30.4}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$CRATE_ROOT/target}"
STANDALONE_SO="${STANDALONE_SO:-$CARGO_TARGET_DIR/debug/libngx_rustscript.so}"

if [[ ! -x "$NGINX_SOURCE_DIR/configure" ]]; then
  echo "NGINX_SOURCE_DIR is not a configured 1.30.4 tree: $NGINX_SOURCE_DIR" >&2
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
if [[ "$BANNER" != *nginx/1.30.4* ]]; then
  echo "emitted nginx is not 1.30.4: $BANNER" >&2
  exit 1
fi
REPORT="$("$NGINX_BIN" -V 2>&1 || true)"
if [[ "$REPORT" != *--with-compat* || "$REPORT" != *--with-http_ssl_module* ]]; then
  echo "emitted nginx is missing required configure flags: $REPORT" >&2
  exit 1
fi

readelf -W --dyn-syms "$MODULE_SO" | grep -q 'OBJECT *GLOBAL.*ngx_rustscript_module_identity'
readelf -W -S "$MODULE_SO" | grep -q '\.ngx_rustscript_module_identity'

DUMP_ADD="$CONF_PREFIX/identity-add.bin"
objcopy --dump-section=.ngx_rustscript_module_identity="$DUMP_ADD" "$MODULE_SO"
if [[ "$(wc -c < "$DUMP_ADD")" -ne 32 ]]; then
  echo "add-dynamic identity section is not 32 bytes" >&2
  exit 1
fi

if [[ -f "$STANDALONE_SO" ]]; then
  DUMP_STANDALONE="$CONF_PREFIX/identity-standalone.bin"
  objcopy --dump-section=.ngx_rustscript_module_identity="$DUMP_STANDALONE" "$STANDALONE_SO"
  if ! cmp -s "$DUMP_ADD" "$DUMP_STANDALONE"; then
    echo "add-dynamic identity does not match standalone cdylib fingerprint" >&2
    xxd "$DUMP_ADD" >&2 || true
    xxd "$DUMP_STANDALONE" >&2 || true
    exit 1
  fi
fi

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
