//! Handwritten configuration-time metadata for production ngx hosts.
//!
//! The pinned `pd-host-function` macro does not emit catalog, phase, or
//! capability data. This module is the import allowlist and config-time
//! validator. Runtime dispatch stays in [`crate::host::bind_ngx_hosts`].

use std::fmt;

use vm::{HostApiBuilder, HostApiCatalog, HostFunctionSchema, HostParamSchema, HostTypeSchema};

/// Import name compiled into access-phase programs for OpenResty-style sleep.
pub const NGX_SLEEP_NAME: &str = "ngx::sleep";

/// Millisecond sleep kept for existing scripts that used integer delays.
pub const NGX_SLEEP_MS_NAME: &str = "ngx::sleep_ms";

pub const NGX_ESCAPE_URI_NAME: &str = "ngx::escape_uri";
pub const NGX_UNESCAPE_URI_NAME: &str = "ngx::unescape_uri";
pub const NGX_ENCODE_ARGS_NAME: &str = "ngx::encode_args";
pub const NGX_DECODE_ARGS_NAME: &str = "ngx::decode_args";
pub const NGX_ENCODE_BASE64_NAME: &str = "ngx::encode_base64";
pub const NGX_DECODE_BASE64_NAME: &str = "ngx::decode_base64";
pub const NGX_DECODE_BASE64MIME_NAME: &str = "ngx::decode_base64mime";
pub const NGX_CRC32_SHORT_NAME: &str = "ngx::crc32_short";
pub const NGX_CRC32_LONG_NAME: &str = "ngx::crc32_long";
pub const NGX_HMAC_SHA1_NAME: &str = "ngx::hmac_sha1";
pub const NGX_MD5_NAME: &str = "ngx::md5";
pub const NGX_MD5_BIN_NAME: &str = "ngx::md5_bin";
pub const NGX_SHA1_BIN_NAME: &str = "ngx::sha1_bin";
pub const NGX_QUOTE_SQL_STR_NAME: &str = "ngx::quote_sql_str";

pub const NGX_TODAY_NAME: &str = "ngx::today";
pub const NGX_TIME_NAME: &str = "ngx::time";
pub const NGX_NOW_NAME: &str = "ngx::now";
pub const NGX_UPDATE_TIME_NAME: &str = "ngx::update_time";
pub const NGX_LOCALTIME_NAME: &str = "ngx::localtime";
pub const NGX_UTCTIME_NAME: &str = "ngx::utctime";
pub const NGX_COOKIE_TIME_NAME: &str = "ngx::cookie_time";
pub const NGX_HTTP_TIME_NAME: &str = "ngx::http_time";
pub const NGX_PARSE_HTTP_TIME_NAME: &str = "ngx::parse_http_time";

pub const NGX_REQ_GET_METHOD_NAME: &str = "ngx::req::get_method";
pub const NGX_REQ_HTTP_VERSION_NAME: &str = "ngx::req::http_version";
pub const NGX_REQ_RAW_HEADER_NAME: &str = "ngx::req::raw_header";
pub const NGX_REQ_GET_HEADERS_NAME: &str = "ngx::req::get_headers";
pub const NGX_REQ_GET_URI_ARGS_NAME: &str = "ngx::req::get_uri_args";
pub const NGX_REQ_GET_POST_ARGS_NAME: &str = "ngx::req::get_post_args";
pub const NGX_REQ_IS_INTERNAL_NAME: &str = "ngx::req::is_internal";
pub const NGX_REQ_START_TIME_NAME: &str = "ngx::req::start_time";
pub const NGX_IS_SUBREQUEST_NAME: &str = "ngx::is_subrequest";
pub const NGX_HEADERS_SENT_NAME: &str = "ngx::headers_sent";
pub const NGX_VAR_GET_NAME: &str = "ngx::var::get";

/// NGINX / OpenResty execution phases that can host a script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NgxPhase {
    Init,
    InitWorker,
    Set,
    Rewrite,
    Access,
    Content,
    HeaderFilter,
    BodyFilter,
    Log,
    Balancer,
    SslCertificate,
    SslSessionFetch,
    SslSessionStore,
    SslClientHello,
    ExitWorker,
    Timer,
}

impl fmt::Display for NgxPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Init => "init",
            Self::InitWorker => "init_worker",
            Self::Set => "set",
            Self::Rewrite => "rewrite",
            Self::Access => "access",
            Self::Content => "content",
            Self::HeaderFilter => "header_filter",
            Self::BodyFilter => "body_filter",
            Self::Log => "log",
            Self::Balancer => "balancer",
            Self::SslCertificate => "ssl_certificate",
            Self::SslSessionFetch => "ssl_session_fetch",
            Self::SslSessionStore => "ssl_session_store",
            Self::SslClientHello => "ssl_client_hello",
            Self::ExitWorker => "exit_worker",
            Self::Timer => "timer",
        })
    }
}

/// Host family grouping used by the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostFamily {
    Timer,
    Codec,
    Time,
    Meta,
    Log,
    Constant,
    Request,
}

/// Whether a host submits a future or returns synchronously.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostSyncKind {
    Sync,
    Async,
}

/// Config-time capability tags. Catalog metadata only; the pinned compiler
/// APIs do not enforce these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCapability {
    Timer,
    Codec,
    Time,
    Meta,
    Log,
    Constant,
    RequestRead,
}

/// One production ngx host. Schema fields stay next to the catalog builder.
pub struct HostManifestEntry {
    pub name: &'static str,
    pub family: HostFamily,
    pub sync: HostSyncKind,
    pub phases: &'static [NgxPhase],
    pub capabilities: &'static [HostCapability],
    pub openresty_name: &'static str,
}

const SLEEP_PHASES: &[NgxPhase] = &[
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::Timer,
    NgxPhase::SslCertificate,
    NgxPhase::SslSessionFetch,
];

/// `ngx.escape_uri` contexts from OpenResty v0.10.29.
const PHASE_ESCAPE_URI: &[NgxPhase] = &[
    NgxPhase::Init,
    NgxPhase::InitWorker,
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Timer,
    NgxPhase::Balancer,
    NgxPhase::SslCertificate,
    NgxPhase::SslSessionFetch,
    NgxPhase::SslSessionStore,
    NgxPhase::ExitWorker,
    NgxPhase::SslClientHello,
];

/// `ngx.unescape_uri` contexts: like escape_uri minus ssl_session_fetch/store.
const PHASE_UNESCAPE_URI: &[NgxPhase] = &[
    NgxPhase::Init,
    NgxPhase::InitWorker,
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Timer,
    NgxPhase::Balancer,
    NgxPhase::SslCertificate,
    NgxPhase::ExitWorker,
    NgxPhase::SslClientHello,
];

/// `ngx.encode_args` has no init / init_worker / ssl_session_* / exit_worker.
const PHASE_ENCODE_ARGS: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Timer,
    NgxPhase::Balancer,
    NgxPhase::SslCertificate,
    NgxPhase::SslClientHello,
];

/// `ngx.decode_args` has ssl_session_fetch/store and ssl_client_hello, not init*.
const PHASE_DECODE_ARGS: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Timer,
    NgxPhase::Balancer,
    NgxPhase::SslCertificate,
    NgxPhase::SslSessionFetch,
    NgxPhase::SslSessionStore,
    NgxPhase::SslClientHello,
];

/// `ngx.decode_base64mime`: like the other remaining codec APIs, but OpenResty
/// v0.10.29 omits ssl_client_hello.
const PHASE_CODEC_MISC: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Timer,
    NgxPhase::Balancer,
    NgxPhase::SslCertificate,
    NgxPhase::SslSessionFetch,
    NgxPhase::SslSessionStore,
];

/// Remaining codec APIs that OpenResty v0.10.29 allows in ssl_client_hello.
const PHASE_CODEC_MISC_SSL_CLIENT_HELLO: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Timer,
    NgxPhase::Balancer,
    NgxPhase::SslCertificate,
    NgxPhase::SslSessionFetch,
    NgxPhase::SslSessionStore,
    NgxPhase::SslClientHello,
];

/// Time APIs: init_worker yes, init no; plus ssl_client_hello and exit_worker.
const PHASE_TIME: &[NgxPhase] = &[
    NgxPhase::InitWorker,
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Timer,
    NgxPhase::Balancer,
    NgxPhase::SslCertificate,
    NgxPhase::SslSessionFetch,
    NgxPhase::SslSessionStore,
    NgxPhase::ExitWorker,
    NgxPhase::SslClientHello,
];

const TIMER_CAPABILITIES: &[HostCapability] = &[HostCapability::Timer];
const CODEC_CAPABILITIES: &[HostCapability] = &[HostCapability::Codec];
const TIME_CAPABILITIES: &[HostCapability] = &[HostCapability::Time];
const META_CAPABILITIES: &[HostCapability] = &[HostCapability::Meta];
const LOG_CAPABILITIES: &[HostCapability] = &[HostCapability::Log];
const CONSTANT_CAPABILITIES: &[HostCapability] = &[HostCapability::Constant];
const REQUEST_READ_CAPABILITIES: &[HostCapability] = &[HostCapability::RequestRead];

/// `ngx.req.get_method` contexts from OpenResty v0.10.29.
const PHASE_REQ_GET_METHOD: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Balancer,
    NgxPhase::Log,
];

/// `ngx.req.http_version` / `ngx.req.raw_header`.
const PHASE_REQ_HTTP_VERSION: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
];

/// `ngx.req.get_uri_args`.
const PHASE_REQ_GET_URI_ARGS: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Balancer,
];

/// `ngx.req.get_post_args`.
const PHASE_REQ_GET_POST_ARGS: &[NgxPhase] = &[
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
];

/// `ngx.req.get_headers`.
const PHASE_REQ_GET_HEADERS: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
];

/// `ngx.req.is_internal`, `ngx.req.start_time`, `ngx.is_subrequest`.
const PHASE_REQ_FLAGS: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
];

/// `ngx.headers_sent`.
const PHASE_HEADERS_SENT: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
];

/// `ngx.var.VARIABLE` read.
const PHASE_VAR_GET: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Balancer,
];

/// OpenResty `ngx.log` / `ngx.get_phase` contexts: every script phase.
const PHASE_ALL: &[NgxPhase] = PHASE_ESCAPE_URI;

/// Core/HTTP constants: OpenResty omits init_worker and exit_worker.
const PHASE_CORE_CONST: &[NgxPhase] = &[
    NgxPhase::Init,
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Timer,
    NgxPhase::Balancer,
    NgxPhase::SslCertificate,
    NgxPhase::SslSessionFetch,
    NgxPhase::SslSessionStore,
    NgxPhase::SslClientHello,
];

/// `ngx.config.subsystem` / `debug` / `prefix` / worker.pid/count/exiting
/// / `nginx_version` (init_worker and exit_worker, unlike `nginx_configure`).
const PHASE_CONFIG_RUNTIME: &[NgxPhase] = &[
    NgxPhase::Init,
    NgxPhase::InitWorker,
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Timer,
    NgxPhase::ExitWorker,
];

/// `ngx.config.nginx_configure` / module version. Omits init_worker/exit_worker.
const PHASE_CONFIG_BUILD: &[NgxPhase] = &[
    NgxPhase::Init,
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Timer,
];

/// `ngx.worker.pids` omits init and init_worker.
const PHASE_WORKER_PIDS: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Timer,
    NgxPhase::ExitWorker,
];

/// `ngx.worker.id` omits init.
const PHASE_WORKER_ID: &[NgxPhase] = &[
    NgxPhase::InitWorker,
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Timer,
    NgxPhase::ExitWorker,
];

const MANIFEST: &[HostManifestEntry] = &[
    HostManifestEntry {
        name: NGX_SLEEP_NAME,
        family: HostFamily::Timer,
        sync: HostSyncKind::Async,
        phases: SLEEP_PHASES,
        capabilities: TIMER_CAPABILITIES,
        openresty_name: "ngx.sleep",
    },
    HostManifestEntry {
        name: NGX_SLEEP_MS_NAME,
        family: HostFamily::Timer,
        sync: HostSyncKind::Async,
        phases: SLEEP_PHASES,
        capabilities: TIMER_CAPABILITIES,
        openresty_name: "ngx.sleep",
    },
    codec_entry(NGX_ESCAPE_URI_NAME, "ngx.escape_uri", PHASE_ESCAPE_URI),
    codec_entry(
        NGX_UNESCAPE_URI_NAME,
        "ngx.unescape_uri",
        PHASE_UNESCAPE_URI,
    ),
    codec_entry(NGX_ENCODE_ARGS_NAME, "ngx.encode_args", PHASE_ENCODE_ARGS),
    codec_entry(NGX_DECODE_ARGS_NAME, "ngx.decode_args", PHASE_DECODE_ARGS),
    codec_entry(
        NGX_ENCODE_BASE64_NAME,
        "ngx.encode_base64",
        PHASE_CODEC_MISC_SSL_CLIENT_HELLO,
    ),
    codec_entry(
        NGX_DECODE_BASE64_NAME,
        "ngx.decode_base64",
        PHASE_CODEC_MISC_SSL_CLIENT_HELLO,
    ),
    codec_entry(
        NGX_DECODE_BASE64MIME_NAME,
        "ngx.decode_base64mime",
        PHASE_CODEC_MISC,
    ),
    codec_entry(
        NGX_CRC32_SHORT_NAME,
        "ngx.crc32_short",
        PHASE_CODEC_MISC_SSL_CLIENT_HELLO,
    ),
    codec_entry(
        NGX_CRC32_LONG_NAME,
        "ngx.crc32_long",
        PHASE_CODEC_MISC_SSL_CLIENT_HELLO,
    ),
    codec_entry(
        NGX_HMAC_SHA1_NAME,
        "ngx.hmac_sha1",
        PHASE_CODEC_MISC_SSL_CLIENT_HELLO,
    ),
    codec_entry(NGX_MD5_NAME, "ngx.md5", PHASE_CODEC_MISC_SSL_CLIENT_HELLO),
    codec_entry(
        NGX_MD5_BIN_NAME,
        "ngx.md5_bin",
        PHASE_CODEC_MISC_SSL_CLIENT_HELLO,
    ),
    codec_entry(
        NGX_SHA1_BIN_NAME,
        "ngx.sha1_bin",
        PHASE_CODEC_MISC_SSL_CLIENT_HELLO,
    ),
    codec_entry(
        NGX_QUOTE_SQL_STR_NAME,
        "ngx.quote_sql_str",
        PHASE_CODEC_MISC_SSL_CLIENT_HELLO,
    ),
    time_entry(NGX_TODAY_NAME, "ngx.today", PHASE_TIME),
    time_entry(NGX_TIME_NAME, "ngx.time", PHASE_TIME),
    time_entry(NGX_NOW_NAME, "ngx.now", PHASE_TIME),
    time_entry(NGX_UPDATE_TIME_NAME, "ngx.update_time", PHASE_TIME),
    time_entry(NGX_LOCALTIME_NAME, "ngx.localtime", PHASE_TIME),
    time_entry(NGX_UTCTIME_NAME, "ngx.utctime", PHASE_TIME),
    time_entry(NGX_COOKIE_TIME_NAME, "ngx.cookie_time", PHASE_TIME),
    time_entry(NGX_HTTP_TIME_NAME, "ngx.http_time", PHASE_TIME),
    time_entry(NGX_PARSE_HTTP_TIME_NAME, "ngx.parse_http_time", PHASE_TIME),
    meta_entry("ngx::get_phase", "ngx.get_phase", PHASE_ALL),
    meta_entry(
        "ngx::config::subsystem",
        "ngx.config.subsystem",
        PHASE_CONFIG_RUNTIME,
    ),
    meta_entry(
        "ngx::config::debug",
        "ngx.config.debug",
        PHASE_CONFIG_RUNTIME,
    ),
    meta_entry(
        "ngx::config::prefix",
        "ngx.config.prefix",
        PHASE_CONFIG_RUNTIME,
    ),
    meta_entry(
        "ngx::config::nginx_version",
        "ngx.config.nginx_version",
        PHASE_CONFIG_RUNTIME,
    ),
    meta_entry(
        "ngx::config::nginx_configure",
        "ngx.config.nginx_configure",
        PHASE_CONFIG_BUILD,
    ),
    meta_entry(
        "ngx::config::ngx_rustscript_version",
        "ngx.config.ngx_lua_version",
        PHASE_CONFIG_BUILD,
    ),
    meta_entry(
        "ngx::worker::exiting",
        "ngx.worker.exiting",
        PHASE_CONFIG_RUNTIME,
    ),
    meta_entry("ngx::worker::pid", "ngx.worker.pid", PHASE_CONFIG_RUNTIME),
    meta_entry("ngx::worker::pids", "ngx.worker.pids", PHASE_WORKER_PIDS),
    meta_entry(
        "ngx::worker::count",
        "ngx.worker.count",
        PHASE_CONFIG_RUNTIME,
    ),
    meta_entry("ngx::worker::id", "ngx.worker.id", PHASE_WORKER_ID),
    log_entry("ngx::log", "ngx.log", PHASE_ALL),
    const_entry("ngx::core::ok", "ngx.OK", PHASE_CORE_CONST),
    const_entry("ngx::core::error", "ngx.ERROR", PHASE_CORE_CONST),
    const_entry("ngx::core::again", "ngx.AGAIN", PHASE_CORE_CONST),
    const_entry("ngx::core::done", "ngx.DONE", PHASE_CORE_CONST),
    const_entry("ngx::core::declined", "ngx.DECLINED", PHASE_CORE_CONST),
    const_entry("ngx::core::null_value", "ngx.null", PHASE_CORE_CONST),
    const_entry("ngx::http::get", "ngx.HTTP_GET", PHASE_CORE_CONST),
    const_entry("ngx::http::head", "ngx.HTTP_HEAD", PHASE_CORE_CONST),
    const_entry("ngx::http::put", "ngx.HTTP_PUT", PHASE_CORE_CONST),
    const_entry("ngx::http::post", "ngx.HTTP_POST", PHASE_CORE_CONST),
    const_entry("ngx::http::delete", "ngx.HTTP_DELETE", PHASE_CORE_CONST),
    const_entry("ngx::http::options", "ngx.HTTP_OPTIONS", PHASE_CORE_CONST),
    const_entry("ngx::http::mkcol", "ngx.HTTP_MKCOL", PHASE_CORE_CONST),
    const_entry("ngx::http::copy", "ngx.HTTP_COPY", PHASE_CORE_CONST),
    const_entry("ngx::http::move", "ngx.HTTP_MOVE", PHASE_CORE_CONST),
    const_entry("ngx::http::propfind", "ngx.HTTP_PROPFIND", PHASE_CORE_CONST),
    const_entry(
        "ngx::http::proppatch",
        "ngx.HTTP_PROPPATCH",
        PHASE_CORE_CONST,
    ),
    const_entry("ngx::http::lock", "ngx.HTTP_LOCK", PHASE_CORE_CONST),
    const_entry("ngx::http::unlock", "ngx.HTTP_UNLOCK", PHASE_CORE_CONST),
    const_entry("ngx::http::patch", "ngx.HTTP_PATCH", PHASE_CORE_CONST),
    const_entry("ngx::http::trace", "ngx.HTTP_TRACE", PHASE_CORE_CONST),
    const_entry(
        "ngx::http::status_continue",
        "ngx.HTTP_CONTINUE",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_switching_protocols",
        "ngx.HTTP_SWITCHING_PROTOCOLS",
        PHASE_CORE_CONST,
    ),
    const_entry("ngx::http::status_ok", "ngx.HTTP_OK", PHASE_CORE_CONST),
    const_entry(
        "ngx::http::status_created",
        "ngx.HTTP_CREATED",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_accepted",
        "ngx.HTTP_ACCEPTED",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_no_content",
        "ngx.HTTP_NO_CONTENT",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_partial_content",
        "ngx.HTTP_PARTIAL_CONTENT",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_special_response",
        "ngx.HTTP_SPECIAL_RESPONSE",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_moved_permanently",
        "ngx.HTTP_MOVED_PERMANENTLY",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_moved_temporarily",
        "ngx.HTTP_MOVED_TEMPORARILY",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_see_other",
        "ngx.HTTP_SEE_OTHER",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_not_modified",
        "ngx.HTTP_NOT_MODIFIED",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_temporary_redirect",
        "ngx.HTTP_TEMPORARY_REDIRECT",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_permanent_redirect",
        "ngx.HTTP_PERMANENT_REDIRECT",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_bad_request",
        "ngx.HTTP_BAD_REQUEST",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_unauthorized",
        "ngx.HTTP_UNAUTHORIZED",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_payment_required",
        "ngx.HTTP_PAYMENT_REQUIRED",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_forbidden",
        "ngx.HTTP_FORBIDDEN",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_not_found",
        "ngx.HTTP_NOT_FOUND",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_not_allowed",
        "ngx.HTTP_NOT_ALLOWED",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_not_acceptable",
        "ngx.HTTP_NOT_ACCEPTABLE",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_request_timeout",
        "ngx.HTTP_REQUEST_TIMEOUT",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_conflict",
        "ngx.HTTP_CONFLICT",
        PHASE_CORE_CONST,
    ),
    const_entry("ngx::http::status_gone", "ngx.HTTP_GONE", PHASE_CORE_CONST),
    const_entry(
        "ngx::http::status_upgrade_required",
        "ngx.HTTP_UPGRADE_REQUIRED",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_too_many_requests",
        "ngx.HTTP_TOO_MANY_REQUESTS",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_illegal",
        "ngx.HTTP_ILLEGAL",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_close",
        "ngx.HTTP_CLOSE",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_internal_server_error",
        "ngx.HTTP_INTERNAL_SERVER_ERROR",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_not_implemented",
        "ngx.HTTP_NOT_IMPLEMENTED",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_bad_gateway",
        "ngx.HTTP_BAD_GATEWAY",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_service_unavailable",
        "ngx.HTTP_SERVICE_UNAVAILABLE",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_gateway_timeout",
        "ngx.HTTP_GATEWAY_TIMEOUT",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_version_not_supported",
        "ngx.HTTP_VERSION_NOT_SUPPORTED",
        PHASE_CORE_CONST,
    ),
    const_entry(
        "ngx::http::status_insufficient_storage",
        "ngx.HTTP_INSUFFICIENT_STORAGE",
        PHASE_CORE_CONST,
    ),
    const_entry("ngx::log_level::stderr", "ngx.STDERR", PHASE_ALL),
    const_entry("ngx::log_level::emerg", "ngx.EMERG", PHASE_ALL),
    const_entry("ngx::log_level::alert", "ngx.ALERT", PHASE_ALL),
    const_entry("ngx::log_level::crit", "ngx.CRIT", PHASE_ALL),
    const_entry("ngx::log_level::error", "ngx.ERR", PHASE_ALL),
    const_entry("ngx::log_level::warn", "ngx.WARN", PHASE_ALL),
    const_entry("ngx::log_level::notice", "ngx.NOTICE", PHASE_ALL),
    const_entry("ngx::log_level::info", "ngx.INFO", PHASE_ALL),
    const_entry("ngx::log_level::debug", "ngx.DEBUG", PHASE_ALL),
    request_entry(
        NGX_REQ_GET_METHOD_NAME,
        "ngx.req.get_method",
        PHASE_REQ_GET_METHOD,
    ),
    request_entry(
        NGX_REQ_HTTP_VERSION_NAME,
        "ngx.req.http_version",
        PHASE_REQ_HTTP_VERSION,
    ),
    request_entry(
        NGX_REQ_RAW_HEADER_NAME,
        "ngx.req.raw_header",
        PHASE_REQ_HTTP_VERSION,
    ),
    request_entry(
        NGX_REQ_GET_HEADERS_NAME,
        "ngx.req.get_headers",
        PHASE_REQ_GET_HEADERS,
    ),
    request_entry(
        NGX_REQ_GET_URI_ARGS_NAME,
        "ngx.req.get_uri_args",
        PHASE_REQ_GET_URI_ARGS,
    ),
    request_entry(
        NGX_REQ_GET_POST_ARGS_NAME,
        "ngx.req.get_post_args",
        PHASE_REQ_GET_POST_ARGS,
    ),
    request_entry(
        NGX_REQ_IS_INTERNAL_NAME,
        "ngx.req.is_internal",
        PHASE_REQ_FLAGS,
    ),
    request_entry(
        NGX_REQ_START_TIME_NAME,
        "ngx.req.start_time",
        PHASE_REQ_FLAGS,
    ),
    request_entry(NGX_IS_SUBREQUEST_NAME, "ngx.is_subrequest", PHASE_REQ_FLAGS),
    request_entry(
        NGX_HEADERS_SENT_NAME,
        "ngx.headers_sent",
        PHASE_HEADERS_SENT,
    ),
    request_entry(NGX_VAR_GET_NAME, "ngx.var.VARIABLE", PHASE_VAR_GET),
];

const fn codec_entry(
    name: &'static str,
    openresty_name: &'static str,
    phases: &'static [NgxPhase],
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Codec,
        sync: HostSyncKind::Sync,
        phases,
        capabilities: CODEC_CAPABILITIES,
        openresty_name,
    }
}

const fn time_entry(
    name: &'static str,
    openresty_name: &'static str,
    phases: &'static [NgxPhase],
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Time,
        sync: HostSyncKind::Sync,
        phases,
        capabilities: TIME_CAPABILITIES,
        openresty_name,
    }
}

const fn meta_entry(
    name: &'static str,
    openresty_name: &'static str,
    phases: &'static [NgxPhase],
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Meta,
        sync: HostSyncKind::Sync,
        phases,
        capabilities: META_CAPABILITIES,
        openresty_name,
    }
}

const fn log_entry(
    name: &'static str,
    openresty_name: &'static str,
    phases: &'static [NgxPhase],
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Log,
        sync: HostSyncKind::Sync,
        phases,
        capabilities: LOG_CAPABILITIES,
        openresty_name,
    }
}

const fn const_entry(
    name: &'static str,
    openresty_name: &'static str,
    phases: &'static [NgxPhase],
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Constant,
        sync: HostSyncKind::Sync,
        phases,
        capabilities: CONSTANT_CAPABILITIES,
        openresty_name,
    }
}

const fn request_entry(
    name: &'static str,
    openresty_name: &'static str,
    phases: &'static [NgxPhase],
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Request,
        sync: HostSyncKind::Sync,
        phases,
        capabilities: REQUEST_READ_CAPABILITIES,
        openresty_name,
    }
}

/// Auditable list of production ngx hosts.
pub fn ngx_host_manifest() -> &'static [HostManifestEntry] {
    MANIFEST
}

fn sleep_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        NGX_SLEEP_NAME,
        vec![HostParamSchema::value("seconds", HostTypeSchema::Number)],
        HostTypeSchema::Null,
    )
}

fn sleep_ms_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        NGX_SLEEP_MS_NAME,
        vec![HostParamSchema::value("ms", HostTypeSchema::Int)],
        HostTypeSchema::Null,
    )
}

fn string_in_string_out(name: &'static str, param: &'static str) -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        name,
        vec![HostParamSchema::value(param, HostTypeSchema::String)],
        HostTypeSchema::String,
    )
}

fn string_in_bytes_out(name: &'static str, param: &'static str) -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        name,
        vec![HostParamSchema::value(param, HostTypeSchema::String)],
        HostTypeSchema::Bytes,
    )
}

fn int_in_string_out(name: &'static str, param: &'static str) -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        name,
        vec![HostParamSchema::value(param, HostTypeSchema::Int)],
        HostTypeSchema::String,
    )
}

fn nullary(name: &str, return_type: HostTypeSchema) -> HostFunctionSchema {
    HostFunctionSchema::with_return(name, Vec::new(), return_type)
}

fn schema_for(name: &str) -> HostFunctionSchema {
    match name {
        NGX_SLEEP_NAME => sleep_schema(),
        NGX_SLEEP_MS_NAME => sleep_ms_schema(),
        NGX_ESCAPE_URI_NAME => HostFunctionSchema::with_return(
            NGX_ESCAPE_URI_NAME,
            vec![
                HostParamSchema::value("uri", HostTypeSchema::String),
                HostParamSchema::value("escape_type", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::String,
        ),
        NGX_UNESCAPE_URI_NAME => string_in_string_out(NGX_UNESCAPE_URI_NAME, "uri"),
        NGX_ENCODE_ARGS_NAME => HostFunctionSchema::with_return(
            NGX_ENCODE_ARGS_NAME,
            vec![HostParamSchema::value("args", HostTypeSchema::Unknown)],
            HostTypeSchema::String,
        ),
        NGX_DECODE_ARGS_NAME => HostFunctionSchema::with_return(
            NGX_DECODE_ARGS_NAME,
            vec![
                HostParamSchema::value("query", HostTypeSchema::String),
                HostParamSchema::value("max_args", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Map(Box::new(HostTypeSchema::Unknown)),
        ),
        NGX_ENCODE_BASE64_NAME => HostFunctionSchema::with_return(
            NGX_ENCODE_BASE64_NAME,
            vec![
                HostParamSchema::value("input", HostTypeSchema::Unknown),
                HostParamSchema::value("no_padding", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::String,
        ),
        NGX_DECODE_BASE64_NAME => HostFunctionSchema::with_return(
            NGX_DECODE_BASE64_NAME,
            vec![HostParamSchema::value("text", HostTypeSchema::String)],
            HostTypeSchema::Optional(Box::new(HostTypeSchema::Bytes)),
        ),
        NGX_DECODE_BASE64MIME_NAME => HostFunctionSchema::with_return(
            NGX_DECODE_BASE64MIME_NAME,
            vec![HostParamSchema::value("text", HostTypeSchema::String)],
            HostTypeSchema::Optional(Box::new(HostTypeSchema::Bytes)),
        ),
        NGX_CRC32_SHORT_NAME => HostFunctionSchema::with_return(
            NGX_CRC32_SHORT_NAME,
            vec![HostParamSchema::value("data", HostTypeSchema::String)],
            HostTypeSchema::Int,
        ),
        NGX_CRC32_LONG_NAME => HostFunctionSchema::with_return(
            NGX_CRC32_LONG_NAME,
            vec![HostParamSchema::value("data", HostTypeSchema::String)],
            HostTypeSchema::Int,
        ),
        NGX_HMAC_SHA1_NAME => HostFunctionSchema::with_return(
            NGX_HMAC_SHA1_NAME,
            vec![
                HostParamSchema::value("secret", HostTypeSchema::String),
                HostParamSchema::value("data", HostTypeSchema::String),
            ],
            HostTypeSchema::Bytes,
        ),
        NGX_MD5_NAME => string_in_string_out(NGX_MD5_NAME, "data"),
        NGX_MD5_BIN_NAME => string_in_bytes_out(NGX_MD5_BIN_NAME, "data"),
        NGX_SHA1_BIN_NAME => string_in_bytes_out(NGX_SHA1_BIN_NAME, "data"),
        NGX_QUOTE_SQL_STR_NAME => string_in_string_out(NGX_QUOTE_SQL_STR_NAME, "value"),
        NGX_TODAY_NAME => nullary(NGX_TODAY_NAME, HostTypeSchema::String),
        NGX_TIME_NAME => nullary(NGX_TIME_NAME, HostTypeSchema::Int),
        NGX_NOW_NAME => nullary(NGX_NOW_NAME, HostTypeSchema::Float),
        NGX_UPDATE_TIME_NAME => nullary(NGX_UPDATE_TIME_NAME, HostTypeSchema::Null),
        NGX_LOCALTIME_NAME => nullary(NGX_LOCALTIME_NAME, HostTypeSchema::String),
        NGX_UTCTIME_NAME => nullary(NGX_UTCTIME_NAME, HostTypeSchema::String),
        NGX_COOKIE_TIME_NAME => int_in_string_out(NGX_COOKIE_TIME_NAME, "sec"),
        NGX_HTTP_TIME_NAME => int_in_string_out(NGX_HTTP_TIME_NAME, "sec"),
        NGX_PARSE_HTTP_TIME_NAME => HostFunctionSchema::with_return(
            NGX_PARSE_HTTP_TIME_NAME,
            vec![HostParamSchema::value("text", HostTypeSchema::String)],
            HostTypeSchema::Optional(Box::new(HostTypeSchema::Int)),
        ),
        NGX_REQ_GET_METHOD_NAME => nullary(NGX_REQ_GET_METHOD_NAME, HostTypeSchema::String),
        NGX_REQ_HTTP_VERSION_NAME => nullary(
            NGX_REQ_HTTP_VERSION_NAME,
            HostTypeSchema::Optional(Box::new(HostTypeSchema::Float)),
        ),
        NGX_REQ_RAW_HEADER_NAME => HostFunctionSchema::with_return(
            NGX_REQ_RAW_HEADER_NAME,
            vec![HostParamSchema::value(
                "no_request_line",
                HostTypeSchema::Unknown,
            )],
            HostTypeSchema::String,
        ),
        NGX_REQ_GET_HEADERS_NAME => HostFunctionSchema::with_return(
            NGX_REQ_GET_HEADERS_NAME,
            vec![
                HostParamSchema::value("max_headers", HostTypeSchema::Unknown),
                HostParamSchema::value("raw", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Map(Box::new(HostTypeSchema::Unknown)),
        ),
        NGX_REQ_GET_URI_ARGS_NAME => HostFunctionSchema::with_return(
            NGX_REQ_GET_URI_ARGS_NAME,
            vec![HostParamSchema::value("max_args", HostTypeSchema::Unknown)],
            HostTypeSchema::Map(Box::new(HostTypeSchema::Unknown)),
        ),
        NGX_REQ_GET_POST_ARGS_NAME => HostFunctionSchema::with_return(
            NGX_REQ_GET_POST_ARGS_NAME,
            vec![HostParamSchema::value("max_args", HostTypeSchema::Unknown)],
            HostTypeSchema::Map(Box::new(HostTypeSchema::Unknown)),
        ),
        NGX_REQ_IS_INTERNAL_NAME => nullary(NGX_REQ_IS_INTERNAL_NAME, HostTypeSchema::Bool),
        NGX_REQ_START_TIME_NAME => nullary(NGX_REQ_START_TIME_NAME, HostTypeSchema::Float),
        NGX_IS_SUBREQUEST_NAME => nullary(NGX_IS_SUBREQUEST_NAME, HostTypeSchema::Bool),
        NGX_HEADERS_SENT_NAME => nullary(NGX_HEADERS_SENT_NAME, HostTypeSchema::Bool),
        NGX_VAR_GET_NAME => HostFunctionSchema::with_return(
            NGX_VAR_GET_NAME,
            vec![HostParamSchema::value("name", HostTypeSchema::String)],
            HostTypeSchema::Optional(Box::new(HostTypeSchema::String)),
        ),
        other => metadata_schema(other)
            .unwrap_or_else(|| panic!("ngx host catalog has no schema for {other}")),
    }
}

fn metadata_schema(name: &str) -> Option<HostFunctionSchema> {
    Some(match name {
        "ngx::log" => HostFunctionSchema::with_return(
            name,
            vec![
                HostParamSchema::value("level", HostTypeSchema::Int),
                HostParamSchema::value("message", HostTypeSchema::String),
            ],
            HostTypeSchema::Null,
        ),
        "ngx::core::null_value" => nullary(name, HostTypeSchema::Null),
        "ngx::get_phase"
        | "ngx::config::subsystem"
        | "ngx::config::prefix"
        | "ngx::config::nginx_configure" => nullary(name, HostTypeSchema::String),
        "ngx::config::debug" | "ngx::worker::exiting" => nullary(name, HostTypeSchema::Bool),
        "ngx::worker::pids" => nullary(name, HostTypeSchema::Array(Box::new(HostTypeSchema::Int))),
        "ngx::config::nginx_version"
        | "ngx::config::ngx_rustscript_version"
        | "ngx::worker::pid"
        | "ngx::worker::id"
        | "ngx::worker::count" => nullary(name, HostTypeSchema::Int),
        name if is_int_constant(name) => nullary(name, HostTypeSchema::Int),
        _ => return None,
    })
}

fn is_int_constant(name: &str) -> bool {
    matches!(
        name,
        "ngx::core::ok"
            | "ngx::core::error"
            | "ngx::core::again"
            | "ngx::core::done"
            | "ngx::core::declined"
            | "ngx::http::get"
            | "ngx::http::head"
            | "ngx::http::put"
            | "ngx::http::post"
            | "ngx::http::delete"
            | "ngx::http::options"
            | "ngx::http::mkcol"
            | "ngx::http::copy"
            | "ngx::http::move"
            | "ngx::http::propfind"
            | "ngx::http::proppatch"
            | "ngx::http::lock"
            | "ngx::http::unlock"
            | "ngx::http::patch"
            | "ngx::http::trace"
            | "ngx::http::status_continue"
            | "ngx::http::status_switching_protocols"
            | "ngx::http::status_ok"
            | "ngx::http::status_created"
            | "ngx::http::status_accepted"
            | "ngx::http::status_no_content"
            | "ngx::http::status_partial_content"
            | "ngx::http::status_special_response"
            | "ngx::http::status_moved_permanently"
            | "ngx::http::status_moved_temporarily"
            | "ngx::http::status_see_other"
            | "ngx::http::status_not_modified"
            | "ngx::http::status_temporary_redirect"
            | "ngx::http::status_permanent_redirect"
            | "ngx::http::status_bad_request"
            | "ngx::http::status_unauthorized"
            | "ngx::http::status_payment_required"
            | "ngx::http::status_forbidden"
            | "ngx::http::status_not_found"
            | "ngx::http::status_not_allowed"
            | "ngx::http::status_not_acceptable"
            | "ngx::http::status_request_timeout"
            | "ngx::http::status_conflict"
            | "ngx::http::status_gone"
            | "ngx::http::status_upgrade_required"
            | "ngx::http::status_too_many_requests"
            | "ngx::http::status_illegal"
            | "ngx::http::status_close"
            | "ngx::http::status_internal_server_error"
            | "ngx::http::status_not_implemented"
            | "ngx::http::status_bad_gateway"
            | "ngx::http::status_service_unavailable"
            | "ngx::http::status_gateway_timeout"
            | "ngx::http::status_version_not_supported"
            | "ngx::http::status_insufficient_storage"
            | "ngx::log_level::stderr"
            | "ngx::log_level::emerg"
            | "ngx::log_level::alert"
            | "ngx::log_level::crit"
            | "ngx::log_level::error"
            | "ngx::log_level::warn"
            | "ngx::log_level::notice"
            | "ngx::log_level::info"
            | "ngx::log_level::debug"
    )
}

/// Compiler catalog for production ngx imports.
///
/// This is compiler import-allowlist metadata, not a runtime binding. The
/// pinned `pd-host-function` macro generates the wrapper, argument conversion,
/// and `submit_host_future` call; it does not emit catalog APIs. Bindings stay
/// exclusively macro-generated via [`crate::host::bind_ngx_hosts`].
pub fn ngx_host_catalog() -> HostApiCatalog {
    let mut builder = HostApiBuilder::new();
    for entry in MANIFEST {
        builder.function(schema_for(entry.name));
    }
    builder
        .build()
        .expect("ngx host catalog must be a valid HostApiCatalog")
}

/// True when `name` is a catalogued host allowed in `phase`.
pub fn host_allowed_in_phase(name: &str, phase: NgxPhase) -> bool {
    MANIFEST
        .iter()
        .find(|entry| entry.name == name)
        .is_some_and(|entry| entry.phases.contains(&phase))
}

/// Look up a production manifest entry by import name.
pub fn manifest_entry(name: &str) -> Option<&'static HostManifestEntry> {
    MANIFEST.iter().find(|entry| entry.name == name)
}
