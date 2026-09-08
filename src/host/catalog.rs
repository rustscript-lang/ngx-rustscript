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

const L1_PHASES: &[NgxPhase] = &[
    NgxPhase::InitWorker,
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Balancer,
    NgxPhase::SslCertificate,
    NgxPhase::SslSessionFetch,
    NgxPhase::SslSessionStore,
    NgxPhase::Timer,
];

const TIMER_CAPABILITIES: &[HostCapability] = &[HostCapability::Timer];
const CODEC_CAPABILITIES: &[HostCapability] = &[HostCapability::Codec];
const TIME_CAPABILITIES: &[HostCapability] = &[HostCapability::Time];

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
    codec_entry(NGX_ESCAPE_URI_NAME, "ngx.escape_uri"),
    codec_entry(NGX_UNESCAPE_URI_NAME, "ngx.unescape_uri"),
    codec_entry(NGX_ENCODE_ARGS_NAME, "ngx.encode_args"),
    codec_entry(NGX_DECODE_ARGS_NAME, "ngx.decode_args"),
    codec_entry(NGX_ENCODE_BASE64_NAME, "ngx.encode_base64"),
    codec_entry(NGX_DECODE_BASE64_NAME, "ngx.decode_base64"),
    codec_entry(NGX_DECODE_BASE64MIME_NAME, "ngx.decode_base64mime"),
    codec_entry(NGX_CRC32_SHORT_NAME, "ngx.crc32_short"),
    codec_entry(NGX_CRC32_LONG_NAME, "ngx.crc32_long"),
    codec_entry(NGX_HMAC_SHA1_NAME, "ngx.hmac_sha1"),
    codec_entry(NGX_MD5_NAME, "ngx.md5"),
    codec_entry(NGX_MD5_BIN_NAME, "ngx.md5_bin"),
    codec_entry(NGX_SHA1_BIN_NAME, "ngx.sha1_bin"),
    codec_entry(NGX_QUOTE_SQL_STR_NAME, "ngx.quote_sql_str"),
    time_entry(NGX_TODAY_NAME, "ngx.today"),
    time_entry(NGX_TIME_NAME, "ngx.time"),
    time_entry(NGX_NOW_NAME, "ngx.now"),
    time_entry(NGX_UPDATE_TIME_NAME, "ngx.update_time"),
    time_entry(NGX_LOCALTIME_NAME, "ngx.localtime"),
    time_entry(NGX_UTCTIME_NAME, "ngx.utctime"),
    time_entry(NGX_COOKIE_TIME_NAME, "ngx.cookie_time"),
    time_entry(NGX_HTTP_TIME_NAME, "ngx.http_time"),
    time_entry(NGX_PARSE_HTTP_TIME_NAME, "ngx.parse_http_time"),
];

const fn codec_entry(name: &'static str, openresty_name: &'static str) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Codec,
        sync: HostSyncKind::Sync,
        phases: L1_PHASES,
        capabilities: CODEC_CAPABILITIES,
        openresty_name,
    }
}

const fn time_entry(name: &'static str, openresty_name: &'static str) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Time,
        sync: HostSyncKind::Sync,
        phases: L1_PHASES,
        capabilities: TIME_CAPABILITIES,
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

fn nullary(name: &'static str, return_type: HostTypeSchema) -> HostFunctionSchema {
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
        other => panic!("ngx host catalog has no schema for {other}"),
    }
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
