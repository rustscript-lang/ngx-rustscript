#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/package_release.sh \
  --version VERSION --arch ARCH --module PATH \
  --nginx PATH --nginx-source DIR --output-dir DIR
EOF
}

VERSION=""
ARCH=""
MODULE=""
NGINX=""
NGINX_SOURCE=""
OUTPUT_DIR=""

while (($#)); do
  case "$1" in
    --version)
      [[ $# -ge 2 ]] || { echo "missing value for --version" >&2; exit 2; }
      VERSION="$2"
      shift 2
      ;;
    --arch)
      [[ $# -ge 2 ]] || { echo "missing value for --arch" >&2; exit 2; }
      ARCH="$2"
      shift 2
      ;;
    --module)
      [[ $# -ge 2 ]] || { echo "missing value for --module" >&2; exit 2; }
      MODULE="$2"
      shift 2
      ;;
    --nginx)
      [[ $# -ge 2 ]] || { echo "missing value for --nginx" >&2; exit 2; }
      NGINX="$2"
      shift 2
      ;;
    --nginx-source)
      [[ $# -ge 2 ]] || { echo "missing value for --nginx-source" >&2; exit 2; }
      NGINX_SOURCE="$2"
      shift 2
      ;;
    --output-dir)
      [[ $# -ge 2 ]] || { echo "missing value for --output-dir" >&2; exit 2; }
      OUTPUT_DIR="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

[[ -n "$VERSION" && -n "$ARCH" && -n "$MODULE" && -n "$NGINX" \
  && -n "$NGINX_SOURCE" && -n "$OUTPUT_DIR" ]] || {
  echo "version, arch, module, nginx, nginx-source, and output-dir are required" >&2
  usage >&2
  exit 2
}

for command in chmod cp dpkg dpkg-deb find git gzip install mktemp readlink sha256sum tar touch; do
  command -v "$command" >/dev/null 2>&1 || {
    echo "required command is unavailable: $command" >&2
    exit 1
  }
done

if [[ ! "$VERSION" =~ ^[0-9][0-9A-Za-z.+:~_-]*$ ]] || ! dpkg --validate-version "$VERSION" >/dev/null 2>&1; then
  echo "invalid version: $VERSION" >&2
  exit 2
fi
if [[ ! "$ARCH" =~ ^[a-z0-9][a-z0-9-]*$ ]]; then
  echo "invalid architecture: $ARCH" >&2
  exit 2
fi
if [[ ! -f "$MODULE" || -L "$MODULE" ]]; then
  echo "module must be a regular, non-symlink file: $MODULE" >&2
  exit 2
fi
if [[ ! -f "$NGINX" || ! -x "$NGINX" || -L "$NGINX" ]]; then
  echo "nginx must be an executable, regular, non-symlink file: $NGINX" >&2
  exit 2
fi
if [[ ! -d "$NGINX_SOURCE" || -L "$NGINX_SOURCE" ]]; then
  echo "nginx-source must be a non-symlink directory: $NGINX_SOURCE" >&2
  exit 2
fi
for required in conf/mime.types html/index.html html/50x.html LICENSE; do
  if [[ ! -f "$NGINX_SOURCE/$required" || -L "$NGINX_SOURCE/$required" ]]; then
    echo "required nginx bundle input is missing or unsafe: $required" >&2
    exit 1
  fi
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
for required in README.md docs examples; do
  [[ -e "$ROOT/$required" ]] || {
    echo "required package input is missing: $required" >&2
    exit 1
  }
done
for tree in docs examples; do
  if [[ -n "$(find "$ROOT/$tree" -type l -print -quit)" ]]; then
    echo "$tree must not contain symbolic links" >&2
    exit 1
  fi
done

SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct)}"
if [[ ! "$SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]]; then
  echo "SOURCE_DATE_EPOCH must be an integer" >&2
  exit 2
fi
export SOURCE_DATE_EPOCH

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
MODULE="$(readlink -f "$MODULE")"
NGINX="$(readlink -f "$NGINX")"
NGINX_SOURCE="$(cd "$NGINX_SOURCE" && pwd)"
MODULE_TOP="ngx-rustscript-module-${VERSION}-linux-${ARCH}"
BUNDLE_TOP="ngx-rustscript-nginx-${VERSION}-nginx-1.31.4-linux-${ARCH}"
MODULE_TGZ="${MODULE_TOP}.tgz"
BUNDLE_TGZ="${BUNDLE_TOP}.tgz"
DEB="nginx-module-rustscript_${VERSION}_${ARCH}.deb"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/ngx-rustscript-package.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

normalize_tree() {
  local root="$1"
  find "$root" -type d -exec chmod 0755 {} +
  find "$root" -type f -exec chmod 0644 {} +
  find "$root" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
}

write_tgz() {
  local parent="$1"
  local top="$2"
  local output="$3"
  tar \
    --sort=name \
    --mtime="@$SOURCE_DATE_EPOCH" \
    --owner=0 \
    --group=0 \
    --numeric-owner \
    --format=gnu \
    -C "$parent" \
    -cf - "$top" | gzip -n > "$output"
}

MODULE_ROOT="$TMP/module/$MODULE_TOP"
install -d -m 0755 "$MODULE_ROOT/lib" "$MODULE_ROOT/docs" "$MODULE_ROOT/examples"
install -m 0644 "$MODULE" "$MODULE_ROOT/lib/ngx_http_rustscript_module.so"
install -m 0644 "$ROOT/README.md" "$MODULE_ROOT/README.md"
cp -a "$ROOT/docs/." "$MODULE_ROOT/docs/"
cp -a "$ROOT/examples/." "$MODULE_ROOT/examples/"
normalize_tree "$MODULE_ROOT"
write_tgz "$TMP/module" "$MODULE_TOP" "$OUTPUT_DIR/$MODULE_TGZ"

BUNDLE_ROOT="$TMP/bundle/$BUNDLE_TOP"
install -d -m 0755 \
  "$BUNDLE_ROOT/bin" \
  "$BUNDLE_ROOT/sbin" \
  "$BUNDLE_ROOT/modules" \
  "$BUNDLE_ROOT/conf" \
  "$BUNDLE_ROOT/html" \
  "$BUNDLE_ROOT/logs" \
  "$BUNDLE_ROOT/temp/client_body" \
  "$BUNDLE_ROOT/temp/proxy" \
  "$BUNDLE_ROOT/temp/fastcgi" \
  "$BUNDLE_ROOT/temp/uwsgi" \
  "$BUNDLE_ROOT/temp/scgi" \
  "$BUNDLE_ROOT/docs" \
  "$BUNDLE_ROOT/examples" \
  "$BUNDLE_ROOT/licenses/nginx"
install -m 0755 "$NGINX" "$BUNDLE_ROOT/sbin/nginx"
install -m 0644 "$MODULE" "$BUNDLE_ROOT/modules/ngx_http_rustscript_module.so"
install -m 0644 "$NGINX_SOURCE/conf/mime.types" "$BUNDLE_ROOT/conf/mime.types"
install -m 0644 "$NGINX_SOURCE/html/index.html" "$BUNDLE_ROOT/html/index.html"
install -m 0644 "$NGINX_SOURCE/html/50x.html" "$BUNDLE_ROOT/html/50x.html"
install -m 0644 "$NGINX_SOURCE/LICENSE" "$BUNDLE_ROOT/licenses/nginx/LICENSE"
install -m 0644 "$ROOT/README.md" "$BUNDLE_ROOT/README.md"
cp -a "$ROOT/docs/." "$BUNDLE_ROOT/docs/"
cp -a "$ROOT/examples/." "$BUNDLE_ROOT/examples/"
cat > "$BUNDLE_ROOT/bin/nginx-rustscript" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
exec "$ROOT/sbin/nginx" -p "$ROOT/" "$@"
EOF
cat > "$BUNDLE_ROOT/conf/nginx.conf" <<'EOF'
load_module modules/ngx_http_rustscript_module.so;

worker_processes auto;
error_log logs/error.log notice;
pid logs/nginx.pid;

events {
    worker_connections 1024;
}

http {
    include mime.types;
    default_type application/octet-stream;
    access_log logs/access.log;
    sendfile on;
    keepalive_timeout 65;

    client_body_temp_path temp/client_body;
    proxy_temp_path temp/proxy;
    fastcgi_temp_path temp/fastcgi;
    uwsgi_temp_path temp/uwsgi;
    scgi_temp_path temp/scgi;

    server {
        listen 8080;
        server_name localhost;

        location / {
            root html;
            index index.html;
        }
    }
}
EOF
normalize_tree "$BUNDLE_ROOT"
chmod 0755 "$BUNDLE_ROOT/bin/nginx-rustscript" "$BUNDLE_ROOT/sbin/nginx"
touch -h -d "@$SOURCE_DATE_EPOCH" "$BUNDLE_ROOT/bin/nginx-rustscript" "$BUNDLE_ROOT/sbin/nginx"
write_tgz "$TMP/bundle" "$BUNDLE_TOP" "$OUTPUT_DIR/$BUNDLE_TGZ"

DEB_ROOT="$TMP/deb"
DOC_ROOT="$DEB_ROOT/usr/share/doc/nginx-module-rustscript"
install -d -m 0755 "$DEB_ROOT/DEBIAN" "$DEB_ROOT/usr/lib/nginx/modules" "$DOC_ROOT/docs" "$DOC_ROOT/examples"
install -m 0644 "$MODULE" "$DEB_ROOT/usr/lib/nginx/modules/ngx_http_rustscript_module.so"
install -m 0644 "$ROOT/README.md" "$DOC_ROOT/README.md"
cp -a "$ROOT/docs/." "$DOC_ROOT/docs/"
cp -a "$ROOT/examples/." "$DOC_ROOT/examples/"
cat > "$DEB_ROOT/DEBIAN/control" <<EOF
Package: nginx-module-rustscript
Version: $VERSION
Section: httpd
Priority: optional
Architecture: $ARCH
Maintainer: fffonion <fffonion@gmail.com>
Depends: libc6, libgcc-s1
Homepage: https://github.com/rustscript-lang/ngx-rustscript
Vcs-Git: https://github.com/rustscript-lang/ngx-rustscript.git
X-Ngx-Rustscript-Nginx-Version: 1.31.4
Description: RustScript HTTP module for NGINX 1.31.4
 Runs RustScript programs in NGINX access, content, and log phases.
 The module requires NGINX 1.31.4 and
 --with-http_ssl_module.
EOF
normalize_tree "$DEB_ROOT"
dpkg-deb --root-owner-group --build "$DEB_ROOT" "$OUTPUT_DIR/$DEB" >/dev/null

(
  cd "$OUTPUT_DIR"
  sha256sum "$MODULE_TGZ" "$BUNDLE_TGZ" "$DEB" > SHA256SUMS
)

printf '%s\n' \
  "$OUTPUT_DIR/$MODULE_TGZ" \
  "$OUTPUT_DIR/$BUNDLE_TGZ" \
  "$OUTPUT_DIR/$DEB" \
  "$OUTPUT_DIR/SHA256SUMS"
