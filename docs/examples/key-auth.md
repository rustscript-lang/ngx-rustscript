# HMAC API-key authentication

[Documentation](../index.md) · [Directives](../directives.md) · [API](../api/codec.md) · [Examples](index.md)

Access-phase example that accepts an `X-API-Key` header, verifies it as an
opaque high-entropy token, and continues the request only when the digest
matches a server-side expected value.

Files:

- `examples/key-auth.rss`
- `examples/key-auth.nginx.conf`
- `examples/key-auth-secrets.conf.example`

## Why HMAC-SHA1

`ngx::hmac_sha1` is the only keyed digest host in this module. The example
uses it as a **token verifier**:

```text
expected = Base64(HMAC-SHA1(pepper, token))  with `=` padding stripped
```

That is not a message signature. The request is not signed, timestamps are
not bound, and the construction does not authenticate method, path, or body.
Use it only for high-entropy opaque API tokens (random bearer secrets), with
TLS in front.

## Status and headers

| Request | Status | Notes |
| --- | --- | --- |
| Missing `X-API-Key` | **401** | Sets `WWW-Authenticate: ApiKey realm="api"` |
| Empty, duplicate, truncated header list, or digest mismatch | **403** | Safe rejection; no `WWW-Authenticate` |
| Valid current or previous digest | continues (**200** from the location) | `ngx::exit(ngx::core::ok())` |
| Missing/empty pepper or expected digest, or a host/VM error | **500** | Fail closed |

The script reads the credential from `ngx::var::get("http_x_api_key")`.
NGINX joins duplicate `X-API-Key` fields into that variable (comma-separated),
so they fail the digest check and return **403**. That is a documented safe
rejection, not a structured duplicate parser.

Only the `X-API-Key` header is accepted. Query-string tokens are unsupported.

## Header stripping

The backend must never see `X-API-Key` or `X_API_Key`. The copy-paste config
clears both:

```nginx
underscores_in_headers off;
proxy_set_header X-API-Key "";
proxy_set_header X_API_Key "";
```

An empty `proxy_set_header` value means NGINX omits the field on the
upstream request. Do not rely on the RSS script to delete the inbound
header; stripping is an nginx proxy setting, proven by a controlled
upstream in `tests/example_key_auth.rs`.

WARNING: `underscores_in_headers on` may admit `X_API_Key` as a distinct
inbound header that still maps toward `$http_x_api_key`. Keep it off (the
NGINX default) and still clear `X_API_Key` on the upstream request.

## Secrets, files, and rotation

- Put pepper and digests in `examples/key-auth-secrets.conf.example` (or a
  deployed copy), mode **0600**, owned by the NGINX master user (typically
  root), outside the web root. The main config in git should keep the
  obvious `REPLACE_*` placeholders and `include` that file.
- Populate `$rss_api_key_pepper`, `$rss_api_key_digest`, and optional
  `$rss_api_key_digest_prev` with `set` / `include` only. Never from
  `$http_*`, `$arg_*`, or `$cookie_*`.
- The shipped template sets `$rss_api_key_digest_prev` to the empty string.
  Leftover `REPLACE_WITH_HIGH_ENTROPY_PEPPER` /
  `REPLACE_WITH_BASE64_NOPAD_HMAC_SHA1_DIGEST` values cannot authorize a
  client token.
- Compute the digest independently (Python):

  ```python
  import base64, hashlib, hmac
  digest = hmac.new(pepper.encode(), token.encode(), hashlib.sha1).digest()
  print(base64.b64encode(digest).rstrip(b"=").decode())
  ```

- Rotation (atomic replace, then reload):
  1. Write a sibling secrets file with the new current digest and the old
     current digest in `rss_api_key_digest_prev`.
  2. `fsync` and rename onto the live include path.
  3. `nginx -s reload`. Current and previous tokens both continue (**200**).
  4. Move clients onto the new token.
  5. Write a sibling file with the new current digest and an empty previous
     digest, rename, reload. The retired token then returns **403**.

## Logging

Do not log the raw token, pepper, or digest. The script only logs a constant
string when configuration is missing. Keep `access_log` / `error_log` formats
free of `$http_x_api_key` and the secret variables.

A second independent known vector is `$request` (the default combined log
includes it). Query-string tokens are unsupported and would appear there
even when `$http_x_api_key` is omitted from the log format.

## Timing

RustScript string `==` is ordinary (not constant-time) equality. This module
does not expose a constant-time compare host, so the example does not claim
one. Matching prefix length of the Base64 digest can theoretically leak
through comparison time.

That is a deployment tradeoff: require high-entropy tokens, terminate TLS at
or in front of NGINX, and rate-limit / lock out brute force. Do not use this
pattern for low-entropy passwords or as a substitute for a real MAC over the
request.

## Copy-paste WARNING

WARNING: `examples/key-auth.nginx.conf` is not a production secret store.
Replace every `REPLACE_*` token. Never paste a live pepper, digest, or API
token into a committed file. Deploy secrets through the 0600 include and
replace that file atomically before reload.
