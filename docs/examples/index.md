# Examples

[Documentation](../index.md) · [Directives](../directives.md) · [API](../api/index.md) · [Execution model](../execution-model.md) · [Security](../security.md) · [Examples](index.md) · [Compatibility](../ngx-api-compatibility.md)

Deployable RSS + NGINX snippets. Replace every `/absolute/path/to/...`
placeholder before `nginx -t`.

| Example | Phase | What it shows |
| --- | --- | --- |
| [Shared-dict fixed-window rate limiter](rate-limit.md) | access | Typed `SharedSafeStoreResult` / `incr`, fail-closed 429 |
| [HMAC API-key authentication](key-auth.md) | access | `ngx::hmac_sha1`, header stripping, secret include |
| [Async I/O → HTTP → SQLite → shared-dict → JSON](async-pipeline.md) | content | Policy grants, one HTTP allow pair, SQLite root, typed store result |

Source files:

- [examples/rate-limit.rss](../../examples/rate-limit.rss) / [examples/rate-limit.nginx.conf](../../examples/rate-limit.nginx.conf)
- [examples/key-auth.rss](../../examples/key-auth.rss) / [examples/key-auth.nginx.conf](../../examples/key-auth.nginx.conf) / [examples/key-auth-secrets.conf.example](../../examples/key-auth-secrets.conf.example)
- [examples/async-pipeline.rss](../../examples/async-pipeline.rss) / [examples/async-pipeline.nginx.conf](../../examples/async-pipeline.nginx.conf)

## Minimal scripts

Import-free access script (runs to `Halted`, then the content phase):

```rss
let x = 1 + 2;
```

One sleep, then the content phase:

```rss
use ngx;
ngx::sleep(0.03);
```

Millisecond sleep for integer delays:

```rss
use ngx;
ngx::sleep_ms(30);
```

Read a request body in `content_by_rss_file` (nullary like OpenResty;
`get_post_args` still takes an explicit argument for the max-args adaptation):

```rss
use ngx;
ngx::req::read_body();
let data = ngx::req::get_body_data().unwrap_or("");
ngx::say(data);
```

Replace the request body in `content_by_rss_file` (memory; `set_body_file`
/ builder APIs are in the [request API](../api/request.md)):

```rss
use ngx;
ngx::req::read_body();
ngx::req::set_body_data("replacement");
ngx::say(ngx::req::get_body_data().unwrap_or(""));
```

Two sequential sleeps:

```rss
use ngx;
ngx::sleep(0.01);
ngx::sleep_ms(10);
```

UDP echo to a connected peer:

```rss
use ngx;
let sock = ngx::socket::udp();
ngx::socket::udp::settimeout(sock, 1000);
ngx::socket::udp::setpeername(sock, "127.0.0.1", 53);
ngx::socket::udp::send(sock, "ping");
ngx::socket::udp::receive(sock, ());
ngx::socket::udp::close(sock);
```

TCP echo to a connected peer:

```rss
use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 1000);
ngx::socket::tcp::connect(sock, "127.0.0.1", 8080);
ngx::socket::tcp::send(sock, "ping");
ngx::socket::tcp::receive(sock, 4);
ngx::socket::tcp::close(sock);
```

TCP keepalive (worker-local pool) and TLS handshake (Tokio/rustls):

```rss
use ngx;
let sock = ngx::socket::tcp();
ngx::socket::tcp::settimeout(sock, 1000);
ngx::socket::tcp::connect(sock, "127.0.0.1", 8080);
let store = ngx::socket::tcp::trustedstore(ca_pem);
ngx::socket::tcp::settrustedstore(sock, store);
ngx::socket::tcp::sslhandshake(sock, (), "localhost", true, ());
ngx::socket::tcp::send(sock, "ping");
ngx::socket::tcp::receive(sock, 4);
ngx::socket::tcp::setkeepalive(sock, 60000, 30);
```
