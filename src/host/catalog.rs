//! Handwritten configuration-time metadata for production ngx hosts.
//!
//! The pinned `pd-host-function` macro does not emit catalog, phase, or
//! capability data. This module is the import allowlist and config-time
//! validator. Runtime dispatch is installed from [`crate::host::ngx_host_modules`]
//! through the restricted exact descriptor registry.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::OnceLock;

use vm::{
    HostApiCatalog, HostFunctionDescriptor, HostFunctionSchema, HostParamPassing, HostParamSchema,
    HostStructField, HostTypeSchema, ResourceTypeKey,
};

/// Import name compiled into access-phase programs for OpenResty-style sleep.
pub const NGX_SLEEP_NAME: &str = "ngx::sleep";

/// Millisecond sleep kept for existing scripts that used integer delays.
pub const NGX_SLEEP_MS_NAME: &str = "ngx::sleep_ms";

pub const NGX_SEMAPHORE_NEW_NAME: &str = "ngx::semaphore::new";
pub const NGX_SEMAPHORE_WAIT_NAME: &str = "ngx::semaphore::wait";
pub const NGX_SEMAPHORE_POST_NAME: &str = "ngx::semaphore::post";
pub const NGX_SEMAPHORE_COUNT_NAME: &str = "ngx::semaphore::count";

/// Registers one owned callback that runs once after `seconds`.
///
/// Consumes the callback through `HostParamPassing::TakeOwned`; the callback VM
/// is isolated and survives the creating request. The bare `ngx::timer`
/// namespace marker is deliberately **not** a callable and must never be
/// catalogued as one.
pub const NGX_TIMER_AT_NAME: &str = "ngx::timer::at";

/// Registers one owned repeating callback driven by `seconds`.
pub const NGX_TIMER_EVERY_NAME: &str = "ngx::timer::every";

/// Number of accepted registrations waiting to begin.
pub const NGX_TIMER_PENDING_COUNT_NAME: &str = "ngx::timer::pending_count";

/// Number of live callback executions, including callbacks waiting on async
/// host work.
pub const NGX_TIMER_RUNNING_COUNT_NAME: &str = "ngx::timer::running_count";

/// Resource type key of the VM-local semaphore handle entry.
///
/// The bare `ngx::semaphore` namespace marker is deliberately **not** a
/// callable and must never be catalogued as one.
pub use crate::semaphore::{NGX_SEMAPHORE_RESOURCE_KEY, semaphore_resource_key};

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
pub const NGX_REQ_READ_BODY_NAME: &str = "ngx::req::read_body";
pub const NGX_REQ_DISCARD_BODY_NAME: &str = "ngx::req::discard_body";
pub const NGX_REQ_GET_BODY_DATA_NAME: &str = "ngx::req::get_body_data";
pub const NGX_REQ_GET_BODY_FILE_NAME: &str = "ngx::req::get_body_file";
pub const NGX_REQ_SET_BODY_DATA_NAME: &str = "ngx::req::set_body_data";
pub const NGX_REQ_SET_BODY_FILE_NAME: &str = "ngx::req::set_body_file";
pub const NGX_REQ_INIT_BODY_NAME: &str = "ngx::req::init_body";
pub const NGX_REQ_APPEND_BODY_NAME: &str = "ngx::req::append_body";
pub const NGX_REQ_FINISH_BODY_NAME: &str = "ngx::req::finish_body";
pub const NGX_REQ_IS_INTERNAL_NAME: &str = "ngx::req::is_internal";
pub const NGX_REQ_START_TIME_NAME: &str = "ngx::req::start_time";
pub const NGX_IS_SUBREQUEST_NAME: &str = "ngx::is_subrequest";
pub const NGX_HEADERS_SENT_NAME: &str = "ngx::headers_sent";
pub const NGX_VAR_GET_NAME: &str = "ngx::var::get";
pub const NGX_REQ_SET_METHOD_NAME: &str = "ngx::req::set_method";
pub const NGX_REQ_SET_URI_NAME: &str = "ngx::req::set_uri";
pub const NGX_REQ_SET_URI_ARGS_NAME: &str = "ngx::req::set_uri_args";
pub const NGX_REQ_SET_HEADER_NAME: &str = "ngx::req::set_header";
pub const NGX_REQ_CLEAR_HEADER_NAME: &str = "ngx::req::clear_header";
pub const NGX_VAR_SET_NAME: &str = "ngx::var::set";
pub const NGX_STATUS_GET_NAME: &str = "ngx::status::get";
pub const NGX_STATUS_SET_NAME: &str = "ngx::status::set";
pub const NGX_HEADER_GET_NAME: &str = "ngx::header::get";
pub const NGX_HEADER_SET_NAME: &str = "ngx::header::set";
pub const NGX_HEADER_SET_ALL_NAME: &str = "ngx::header::set_all";
pub const NGX_HEADER_CLEAR_NAME: &str = "ngx::header::clear";
pub const NGX_EXIT_NAME: &str = "ngx::exit";
pub const NGX_REDIRECT_NAME: &str = "ngx::redirect";
pub const NGX_CTX_GET_NAME: &str = "ngx::ctx::get";
pub const NGX_CTX_SET_NAME: &str = "ngx::ctx::set";
pub const NGX_CTX_DELETE_NAME: &str = "ngx::ctx::delete";
pub const NGX_CTX_CLEAR_NAME: &str = "ngx::ctx::clear";
pub const NGX_SOCKET_UDP_NAME: &str = "ngx::socket::udp";
pub const NGX_SOCKET_UDP_BIND_NAME: &str = "ngx::socket::udp::bind";
pub const NGX_SOCKET_UDP_SETPEERNAME_NAME: &str = "ngx::socket::udp::setpeername";
pub const NGX_SOCKET_UDP_SEND_NAME: &str = "ngx::socket::udp::send";
pub const NGX_SOCKET_UDP_RECEIVE_NAME: &str = "ngx::socket::udp::receive";
pub const NGX_SOCKET_UDP_CLOSE_NAME: &str = "ngx::socket::udp::close";
pub const NGX_SOCKET_UDP_SETTIMEOUT_NAME: &str = "ngx::socket::udp::settimeout";
pub const NGX_SOCKET_UDP_RESOURCE_KEY: &str = "ngx.socket.udp";
pub const NGX_SOCKET_TCP_NAME: &str = "ngx::socket::tcp";
pub const NGX_SOCKET_TCP_CONNECT_NAME: &str = "ngx::socket::tcp::connect";
pub const NGX_SOCKET_CONNECT_NAME: &str = "ngx::socket::connect";
pub const NGX_SOCKET_TCP_SEND_NAME: &str = "ngx::socket::tcp::send";
pub const NGX_SOCKET_TCP_RECEIVE_NAME: &str = "ngx::socket::tcp::receive";
pub const NGX_SOCKET_TCP_RECEIVEANY_NAME: &str = "ngx::socket::tcp::receiveany";
pub const NGX_SOCKET_TCP_RECEIVEUNTIL_NAME: &str = "ngx::socket::tcp::receiveuntil";
pub const NGX_SOCKET_TCP_CLOSE_NAME: &str = "ngx::socket::tcp::close";
pub const NGX_SOCKET_TCP_SETTIMEOUT_NAME: &str = "ngx::socket::tcp::settimeout";
pub const NGX_SOCKET_TCP_SETKEEPALIVE_NAME: &str = "ngx::socket::tcp::setkeepalive";
pub const NGX_SOCKET_TCP_GETREUSEDTIMES_NAME: &str = "ngx::socket::tcp::getreusedtimes";
pub const NGX_SOCKET_TCP_SSLHANDSHAKE_NAME: &str = "ngx::socket::tcp::sslhandshake";
pub const NGX_SOCKET_TCP_SETTRUSTEDSTORE_NAME: &str = "ngx::socket::tcp::settrustedstore";
pub const NGX_SOCKET_TCP_SETCLIENTCERT_NAME: &str = "ngx::socket::tcp::setclientcert";
pub const NGX_SOCKET_TCP_TRUSTEDSTORE_NAME: &str = "ngx::socket::tcp::trustedstore";
pub const NGX_SOCKET_TCP_CLIENTCERT_NAME: &str = "ngx::socket::tcp::clientcert";
pub const NGX_SOCKET_TCP_RESOURCE_KEY: &str = "ngx.socket.tcp";
pub const NGX_SOCKET_TLS_TRUSTEDSTORE_RESOURCE_KEY: &str = "ngx.socket.tls.trustedstore";
pub const NGX_SOCKET_TLS_CLIENTCERT_RESOURCE_KEY: &str = "ngx.socket.tls.clientcert";

pub const NGX_SHARED_DICT_NAME: &str = "ngx::shared::dict";
pub const NGX_SHARED_GET_NAME: &str = "ngx::shared::get";
pub const NGX_SHARED_GET_STALE_NAME: &str = "ngx::shared::get_stale";
pub const NGX_SHARED_SET_NAME: &str = "ngx::shared::set";
pub const NGX_SHARED_SAFE_SET_NAME: &str = "ngx::shared::safe_set";
pub const NGX_SHARED_ADD_NAME: &str = "ngx::shared::add";
pub const NGX_SHARED_SAFE_ADD_NAME: &str = "ngx::shared::safe_add";
pub const NGX_SHARED_REPLACE_NAME: &str = "ngx::shared::replace";
pub const NGX_SHARED_DELETE_NAME: &str = "ngx::shared::delete";
pub const NGX_SHARED_INCR_NAME: &str = "ngx::shared::incr";
pub const NGX_SHARED_TTL_NAME: &str = "ngx::shared::ttl";
pub const NGX_SHARED_EXPIRE_NAME: &str = "ngx::shared::expire";
pub const NGX_SHARED_CAPACITY_NAME: &str = "ngx::shared::capacity";
pub const NGX_SHARED_FREE_SPACE_NAME: &str = "ngx::shared::free_space";
pub const NGX_SHARED_LPUSH_NAME: &str = "ngx::shared::lpush";
pub const NGX_SHARED_RPUSH_NAME: &str = "ngx::shared::rpush";
pub const NGX_SHARED_LPOP_NAME: &str = "ngx::shared::lpop";
pub const NGX_SHARED_RPOP_NAME: &str = "ngx::shared::rpop";
pub const NGX_SHARED_LLEN_NAME: &str = "ngx::shared::llen";
pub const NGX_SHARED_FLUSH_ALL_NAME: &str = "ngx::shared::flush_all";
pub const NGX_SHARED_FLUSH_EXPIRED_NAME: &str = "ngx::shared::flush_expired";
pub const NGX_SHARED_GET_KEYS_NAME: &str = "ngx::shared::get_keys";

/// Catalog schema version for fingerprint goldens. Bump together with
/// [`NGX_HOST_CATALOG_FINGERPRINT`] when named structs, resource keys, or
/// function shapes change.
pub const NGX_HOST_CATALOG_SCHEMA_VERSION: u32 = 3;

/// Fingerprint of schema version 1: named request/codec envelopes with
/// `Unknown` shared-dict result carriers.
pub const NGX_HOST_CATALOG_FINGERPRINT_V1: u64 = 0x5e37_8285_8fa0_b584;

/// Fingerprint of schema version 2 under the preceding RustScript descriptor
/// fingerprint contract.
pub const NGX_HOST_CATALOG_FINGERPRINT_V2: u64 = 0x10c4_07b2_b827_eaff;

/// Fingerprint of the current schema version. The host_catalog drift gate
/// compares `ngx_host_catalog().fingerprint()` against this constant.
pub const NGX_HOST_CATALOG_FINGERPRINT: u64 = 0xc54f_427f_8ef6_f5d4;

pub const NGX_SEND_HEADERS_NAME: &str = "ngx::send_headers";
pub const NGX_PRINT_NAME: &str = "ngx::print";
pub const NGX_SAY_NAME: &str = "ngx::say";
pub const NGX_FLUSH_NAME: &str = "ngx::flush";
pub const NGX_EOF_NAME: &str = "ngx::eof";

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
    Semaphore,
    Codec,
    Time,
    Meta,
    Log,
    Constant,
    Request,
    RequestCtx,
    Response,
    Control,
    SharedDict,
    Cosocket,
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
    Semaphore,
    Codec,
    Time,
    Meta,
    Log,
    Constant,
    RequestRead,
    RequestWrite,
    RequestCtx,
    ResponseRead,
    ResponseWrite,
    Control,
    SharedDict,
    Cosocket,
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
const SEMAPHORE_CAPABILITIES: &[HostCapability] = &[HostCapability::Semaphore];
const CODEC_CAPABILITIES: &[HostCapability] = &[HostCapability::Codec];
const TIME_CAPABILITIES: &[HostCapability] = &[HostCapability::Time];
const META_CAPABILITIES: &[HostCapability] = &[HostCapability::Meta];
const LOG_CAPABILITIES: &[HostCapability] = &[HostCapability::Log];
const CONSTANT_CAPABILITIES: &[HostCapability] = &[HostCapability::Constant];
const REQUEST_READ_CAPABILITIES: &[HostCapability] = &[HostCapability::RequestRead];
const REQUEST_WRITE_CAPABILITIES: &[HostCapability] = &[HostCapability::RequestWrite];
const REQUEST_CTX_CAPABILITIES: &[HostCapability] = &[HostCapability::RequestCtx];
const RESPONSE_READ_CAPABILITIES: &[HostCapability] = &[HostCapability::ResponseRead];
const RESPONSE_WRITE_CAPABILITIES: &[HostCapability] = &[HostCapability::ResponseWrite];
const CONTROL_CAPABILITIES: &[HostCapability] = &[HostCapability::Control];
const SHARED_DICT_CAPABILITIES: &[HostCapability] = &[HostCapability::SharedDict];
const COSOCKET_CAPABILITIES: &[HostCapability] = &[HostCapability::Cosocket];
const PHASE_SOCKET_UDP: &[NgxPhase] = SLEEP_PHASES;
const PHASE_SEMAPHORE: &[NgxPhase] = &[NgxPhase::Access, NgxPhase::Content];

/// `ngx::timer` registration and count hosts run in Access and Content only.
///
/// Log registration is rejected at configuration compile: the phase set below
/// omits Log and the closed Log allowlist rejects the family independently.
const PHASE_TIMER_HOSTS: &[NgxPhase] = &[NgxPhase::Access, NgxPhase::Content];

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

/// `ngx.req.read_body` / `ngx.req.discard_body`.
const PHASE_REQ_READ_BODY: &[NgxPhase] = &[NgxPhase::Rewrite, NgxPhase::Access, NgxPhase::Content];

/// `ngx.req.get_body_data`.
const PHASE_REQ_GET_BODY_DATA: &[NgxPhase] = &[
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::Log,
];

/// `ngx.req.get_body_file`.
const PHASE_REQ_GET_BODY_FILE: &[NgxPhase] =
    &[NgxPhase::Rewrite, NgxPhase::Access, NgxPhase::Content];

/// `ngx.req.set_body_data` / `set_body_file` / builder APIs.
const PHASE_REQ_SET_BODY: &[NgxPhase] = &[NgxPhase::Rewrite, NgxPhase::Access, NgxPhase::Content];

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

/// `ngx.req.set_method`.
const PHASE_REQ_SET_METHOD: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
];

/// `ngx.req.set_uri` / `ngx.req.set_uri_args`.
const PHASE_REQ_SET_URI: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
];

/// `ngx.req.set_header` / `ngx.req.clear_header`.
const PHASE_REQ_SET_HEADER: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Balancer,
];

/// `ngx.status` / `ngx.header.HEADER`.
const PHASE_RESPONSE_META: &[NgxPhase] = &[
    NgxPhase::Set,
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::BodyFilter,
    NgxPhase::Log,
    NgxPhase::Balancer,
];

/// `ngx.send_headers` / `ngx.print` / `ngx.say` / `ngx.flush` / `ngx.eof`.
const PHASE_OUTPUT: &[NgxPhase] = &[NgxPhase::Content];

/// `ngx.exit`.
const PHASE_EXIT: &[NgxPhase] = &[
    NgxPhase::Rewrite,
    NgxPhase::Access,
    NgxPhase::Content,
    NgxPhase::HeaderFilter,
    NgxPhase::Timer,
    NgxPhase::Balancer,
    NgxPhase::SslCertificate,
    NgxPhase::SslSessionFetch,
    NgxPhase::SslSessionStore,
];

/// `ngx.redirect`.
const PHASE_REDIRECT: &[NgxPhase] = &[NgxPhase::Rewrite, NgxPhase::Access, NgxPhase::Content];

/// OpenResty v0.10.29 `ngx.ctx` contexts. Configuration phases are excluded.
const PHASE_CTX: &[NgxPhase] = &[
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

/// `ngx.shared.DICT` object lookup, including init / init_worker / exit_worker.
const PHASE_DICT_LOOKUP: &[NgxPhase] = PHASE_ESCAPE_URI;

/// `ngx.shared.DICT.get` / `get_stale`: no init / init_worker / exit_worker.
const PHASE_DICT_GET: &[NgxPhase] = PHASE_CTX;

/// Shared-dict writers: OpenResty allows `init_by_lua*` but not worker init/exit.
const PHASE_DICT_WRITE: &[NgxPhase] = &[
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
    semaphore_entry(
        NGX_SEMAPHORE_NEW_NAME,
        HostSyncKind::Sync,
        "ngx.semaphore.new",
    ),
    semaphore_entry(
        NGX_SEMAPHORE_WAIT_NAME,
        HostSyncKind::Async,
        "ngx.semaphore.wait",
    ),
    semaphore_entry(
        NGX_SEMAPHORE_POST_NAME,
        HostSyncKind::Sync,
        "ngx.semaphore.post",
    ),
    semaphore_entry(
        NGX_SEMAPHORE_COUNT_NAME,
        HostSyncKind::Sync,
        "ngx.semaphore.count",
    ),
    timer_host_entry(NGX_TIMER_AT_NAME, "ngx.timer.at"),
    timer_host_entry(NGX_TIMER_EVERY_NAME, "ngx.timer.every"),
    timer_host_entry(NGX_TIMER_PENDING_COUNT_NAME, "ngx.timer.pending_count"),
    timer_host_entry(NGX_TIMER_RUNNING_COUNT_NAME, "ngx.timer.running_count"),
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
    request_async_entry(
        NGX_REQ_READ_BODY_NAME,
        "ngx.req.read_body",
        PHASE_REQ_READ_BODY,
    ),
    request_entry(
        NGX_REQ_DISCARD_BODY_NAME,
        "ngx.req.discard_body",
        PHASE_REQ_READ_BODY,
    ),
    request_entry(
        NGX_REQ_GET_BODY_DATA_NAME,
        "ngx.req.get_body_data",
        PHASE_REQ_GET_BODY_DATA,
    ),
    request_entry(
        NGX_REQ_GET_BODY_FILE_NAME,
        "ngx.req.get_body_file",
        PHASE_REQ_GET_BODY_FILE,
    ),
    request_write_entry(
        NGX_REQ_SET_BODY_DATA_NAME,
        "ngx.req.set_body_data",
        PHASE_REQ_SET_BODY,
    ),
    request_write_entry(
        NGX_REQ_SET_BODY_FILE_NAME,
        "ngx.req.set_body_file",
        PHASE_REQ_SET_BODY,
    ),
    request_write_entry(
        NGX_REQ_INIT_BODY_NAME,
        "ngx.req.init_body",
        PHASE_REQ_SET_BODY,
    ),
    request_write_entry(
        NGX_REQ_APPEND_BODY_NAME,
        "ngx.req.append_body",
        PHASE_REQ_SET_BODY,
    ),
    request_write_entry(
        NGX_REQ_FINISH_BODY_NAME,
        "ngx.req.finish_body",
        PHASE_REQ_SET_BODY,
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
    request_write_entry(
        NGX_REQ_SET_METHOD_NAME,
        "ngx.req.set_method",
        PHASE_REQ_SET_METHOD,
    ),
    request_write_entry(NGX_REQ_SET_URI_NAME, "ngx.req.set_uri", PHASE_REQ_SET_URI),
    request_write_entry(
        NGX_REQ_SET_URI_ARGS_NAME,
        "ngx.req.set_uri_args",
        PHASE_REQ_SET_URI,
    ),
    request_write_entry(
        NGX_REQ_SET_HEADER_NAME,
        "ngx.req.set_header",
        PHASE_REQ_SET_HEADER,
    ),
    request_write_entry(
        NGX_REQ_CLEAR_HEADER_NAME,
        "ngx.req.clear_header",
        PHASE_REQ_SET_HEADER,
    ),
    request_write_entry(NGX_VAR_SET_NAME, "ngx.var.VARIABLE", PHASE_VAR_GET),
    response_read_entry(NGX_STATUS_GET_NAME, "ngx.status", PHASE_RESPONSE_META),
    response_write_entry(NGX_STATUS_SET_NAME, "ngx.status", PHASE_RESPONSE_META),
    response_read_entry(
        NGX_HEADER_GET_NAME,
        "ngx.header.HEADER",
        PHASE_RESPONSE_META,
    ),
    response_write_entry(
        NGX_HEADER_SET_NAME,
        "ngx.header.HEADER",
        PHASE_RESPONSE_META,
    ),
    response_write_entry(
        NGX_HEADER_SET_ALL_NAME,
        "ngx.header.HEADER",
        PHASE_RESPONSE_META,
    ),
    response_write_entry(
        NGX_HEADER_CLEAR_NAME,
        "ngx.header.HEADER",
        PHASE_RESPONSE_META,
    ),
    control_entry(NGX_EXIT_NAME, "ngx.exit", PHASE_EXIT),
    control_entry(NGX_REDIRECT_NAME, "ngx.redirect", PHASE_REDIRECT),
    ctx_entry(NGX_CTX_GET_NAME, PHASE_CTX),
    ctx_entry(NGX_CTX_SET_NAME, PHASE_CTX),
    ctx_entry(NGX_CTX_DELETE_NAME, PHASE_CTX),
    ctx_entry(NGX_CTX_CLEAR_NAME, PHASE_CTX),
    shared_dict_entry(NGX_SHARED_DICT_NAME, "ngx.shared.DICT", PHASE_DICT_LOOKUP),
    shared_dict_entry(NGX_SHARED_GET_NAME, "ngx.shared.DICT.get", PHASE_DICT_GET),
    shared_dict_entry(
        NGX_SHARED_GET_STALE_NAME,
        "ngx.shared.DICT.get_stale",
        PHASE_DICT_GET,
    ),
    shared_dict_entry(NGX_SHARED_SET_NAME, "ngx.shared.DICT.set", PHASE_DICT_WRITE),
    shared_dict_entry(
        NGX_SHARED_SAFE_SET_NAME,
        "ngx.shared.DICT.safe_set",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(NGX_SHARED_ADD_NAME, "ngx.shared.DICT.add", PHASE_DICT_WRITE),
    shared_dict_entry(
        NGX_SHARED_SAFE_ADD_NAME,
        "ngx.shared.DICT.safe_add",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(
        NGX_SHARED_REPLACE_NAME,
        "ngx.shared.DICT.replace",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(
        NGX_SHARED_DELETE_NAME,
        "ngx.shared.DICT.delete",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(
        NGX_SHARED_INCR_NAME,
        "ngx.shared.DICT.incr",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(NGX_SHARED_TTL_NAME, "ngx.shared.DICT.ttl", PHASE_DICT_WRITE),
    shared_dict_entry(
        NGX_SHARED_EXPIRE_NAME,
        "ngx.shared.DICT.expire",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(
        NGX_SHARED_CAPACITY_NAME,
        "ngx.shared.DICT.capacity",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(
        NGX_SHARED_FREE_SPACE_NAME,
        "ngx.shared.DICT.free_space",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(
        NGX_SHARED_LPUSH_NAME,
        "ngx.shared.DICT.lpush",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(
        NGX_SHARED_RPUSH_NAME,
        "ngx.shared.DICT.rpush",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(
        NGX_SHARED_LPOP_NAME,
        "ngx.shared.DICT.lpop",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(
        NGX_SHARED_RPOP_NAME,
        "ngx.shared.DICT.rpop",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(
        NGX_SHARED_LLEN_NAME,
        "ngx.shared.DICT.llen",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(
        NGX_SHARED_FLUSH_ALL_NAME,
        "ngx.shared.DICT.flush_all",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(
        NGX_SHARED_FLUSH_EXPIRED_NAME,
        "ngx.shared.DICT.flush_expired",
        PHASE_DICT_WRITE,
    ),
    shared_dict_entry(
        NGX_SHARED_GET_KEYS_NAME,
        "ngx.shared.DICT.get_keys",
        PHASE_DICT_WRITE,
    ),
    cosocket_entry(NGX_SOCKET_UDP_NAME, HostSyncKind::Sync, "ngx.socket.udp"),
    cosocket_entry(
        NGX_SOCKET_UDP_BIND_NAME,
        HostSyncKind::Async,
        "ngx.socket.udp",
    ),
    cosocket_entry(
        NGX_SOCKET_UDP_SETPEERNAME_NAME,
        HostSyncKind::Async,
        "ngx.socket.udp",
    ),
    cosocket_entry(
        NGX_SOCKET_UDP_SEND_NAME,
        HostSyncKind::Async,
        "ngx.socket.udp",
    ),
    cosocket_entry(
        NGX_SOCKET_UDP_RECEIVE_NAME,
        HostSyncKind::Async,
        "ngx.socket.udp",
    ),
    cosocket_entry(
        NGX_SOCKET_UDP_CLOSE_NAME,
        HostSyncKind::Sync,
        "ngx.socket.udp",
    ),
    cosocket_entry(
        NGX_SOCKET_UDP_SETTIMEOUT_NAME,
        HostSyncKind::Sync,
        "ngx.socket.udp",
    ),
    cosocket_entry(NGX_SOCKET_TCP_NAME, HostSyncKind::Sync, "ngx.socket.tcp"),
    cosocket_entry(
        NGX_SOCKET_TCP_CONNECT_NAME,
        HostSyncKind::Async,
        "ngx.socket.tcp",
    ),
    cosocket_entry(
        NGX_SOCKET_CONNECT_NAME,
        HostSyncKind::Async,
        "ngx.socket.connect",
    ),
    cosocket_entry(
        NGX_SOCKET_TCP_SEND_NAME,
        HostSyncKind::Async,
        "ngx.socket.tcp",
    ),
    cosocket_entry(
        NGX_SOCKET_TCP_RECEIVE_NAME,
        HostSyncKind::Async,
        "ngx.socket.tcp",
    ),
    cosocket_entry(
        NGX_SOCKET_TCP_RECEIVEANY_NAME,
        HostSyncKind::Async,
        "ngx.socket.tcp",
    ),
    cosocket_entry(
        NGX_SOCKET_TCP_RECEIVEUNTIL_NAME,
        HostSyncKind::Async,
        "ngx.socket.tcp",
    ),
    cosocket_entry(
        NGX_SOCKET_TCP_CLOSE_NAME,
        HostSyncKind::Sync,
        "ngx.socket.tcp",
    ),
    cosocket_entry(
        NGX_SOCKET_TCP_SETTIMEOUT_NAME,
        HostSyncKind::Sync,
        "ngx.socket.tcp",
    ),
    cosocket_entry(
        NGX_SOCKET_TCP_SETKEEPALIVE_NAME,
        HostSyncKind::Sync,
        "ngx.socket.tcp",
    ),
    cosocket_entry(
        NGX_SOCKET_TCP_GETREUSEDTIMES_NAME,
        HostSyncKind::Sync,
        "ngx.socket.tcp",
    ),
    cosocket_entry(
        NGX_SOCKET_TCP_SSLHANDSHAKE_NAME,
        HostSyncKind::Async,
        "ngx.socket.tcp",
    ),
    cosocket_entry(
        NGX_SOCKET_TCP_SETTRUSTEDSTORE_NAME,
        HostSyncKind::Sync,
        "ngx.socket.tcp",
    ),
    cosocket_entry(
        NGX_SOCKET_TCP_SETCLIENTCERT_NAME,
        HostSyncKind::Sync,
        "ngx.socket.tcp",
    ),
    cosocket_entry(
        NGX_SOCKET_TCP_TRUSTEDSTORE_NAME,
        HostSyncKind::Sync,
        "ngx.socket.tcp",
    ),
    cosocket_entry(
        NGX_SOCKET_TCP_CLIENTCERT_NAME,
        HostSyncKind::Sync,
        "ngx.socket.tcp",
    ),
    response_write_entry(NGX_SEND_HEADERS_NAME, "ngx.send_headers", PHASE_OUTPUT),
    response_write_entry(NGX_PRINT_NAME, "ngx.print", PHASE_OUTPUT),
    response_write_entry(NGX_SAY_NAME, "ngx.say", PHASE_OUTPUT),
    HostManifestEntry {
        name: NGX_FLUSH_NAME,
        family: HostFamily::Response,
        sync: HostSyncKind::Async,
        phases: PHASE_OUTPUT,
        capabilities: RESPONSE_WRITE_CAPABILITIES,
        openresty_name: "ngx.flush",
    },
    response_write_entry(NGX_EOF_NAME, "ngx.eof", PHASE_OUTPUT),
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

const fn request_async_entry(
    name: &'static str,
    openresty_name: &'static str,
    phases: &'static [NgxPhase],
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Request,
        sync: HostSyncKind::Async,
        phases,
        capabilities: REQUEST_READ_CAPABILITIES,
        openresty_name,
    }
}

const fn request_write_entry(
    name: &'static str,
    openresty_name: &'static str,
    phases: &'static [NgxPhase],
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Request,
        sync: HostSyncKind::Sync,
        phases,
        capabilities: REQUEST_WRITE_CAPABILITIES,
        openresty_name,
    }
}

const fn response_read_entry(
    name: &'static str,
    openresty_name: &'static str,
    phases: &'static [NgxPhase],
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Response,
        sync: HostSyncKind::Sync,
        phases,
        capabilities: RESPONSE_READ_CAPABILITIES,
        openresty_name,
    }
}

const fn response_write_entry(
    name: &'static str,
    openresty_name: &'static str,
    phases: &'static [NgxPhase],
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Response,
        sync: HostSyncKind::Sync,
        phases,
        capabilities: RESPONSE_WRITE_CAPABILITIES,
        openresty_name,
    }
}

const fn control_entry(
    name: &'static str,
    openresty_name: &'static str,
    phases: &'static [NgxPhase],
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Control,
        sync: HostSyncKind::Sync,
        phases,
        capabilities: CONTROL_CAPABILITIES,
        openresty_name,
    }
}

const fn ctx_entry(name: &'static str, phases: &'static [NgxPhase]) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::RequestCtx,
        sync: HostSyncKind::Sync,
        phases,
        capabilities: REQUEST_CTX_CAPABILITIES,
        openresty_name: "ngx.ctx",
    }
}

const fn shared_dict_entry(
    name: &'static str,
    openresty_name: &'static str,
    phases: &'static [NgxPhase],
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::SharedDict,
        sync: HostSyncKind::Sync,
        phases,
        capabilities: SHARED_DICT_CAPABILITIES,
        openresty_name,
    }
}

const fn semaphore_entry(
    name: &'static str,
    sync: HostSyncKind,
    openresty_name: &'static str,
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Semaphore,
        sync,
        phases: PHASE_SEMAPHORE,
        capabilities: SEMAPHORE_CAPABILITIES,
        openresty_name,
    }
}

/// One `ngx::timer` family member.
///
/// Every member is synchronous at the host boundary: `at`/`every` accept the
/// registration and return, and the counters read the worker registry. Callback
/// execution happens on a timer-owned VM under [`NgxPhase::Timer`], never on the
/// registering request.
const fn timer_host_entry(name: &'static str, openresty_name: &'static str) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Timer,
        sync: HostSyncKind::Sync,
        phases: PHASE_TIMER_HOSTS,
        capabilities: TIMER_CAPABILITIES,
        openresty_name,
    }
}

const fn cosocket_entry(
    name: &'static str,
    sync: HostSyncKind,
    openresty_name: &'static str,
) -> HostManifestEntry {
    HostManifestEntry {
        name,
        family: HostFamily::Cosocket,
        sync,
        phases: PHASE_SOCKET_UDP,
        capabilities: COSOCKET_CAPABILITIES,
        openresty_name,
    }
}

pub fn udp_socket_resource_key() -> ResourceTypeKey {
    ResourceTypeKey::new(NGX_SOCKET_UDP_RESOURCE_KEY)
        .expect("ngx.socket.udp is a valid resource type key")
}

pub fn tcp_socket_resource_key() -> ResourceTypeKey {
    ResourceTypeKey::new(NGX_SOCKET_TCP_RESOURCE_KEY)
        .expect("ngx.socket.tcp is a valid resource type key")
}

pub fn tls_trustedstore_resource_key() -> ResourceTypeKey {
    ResourceTypeKey::new(NGX_SOCKET_TLS_TRUSTEDSTORE_RESOURCE_KEY)
        .expect("ngx.socket.tls.trustedstore is a valid resource type key")
}

pub fn tls_clientcert_resource_key() -> ResourceTypeKey {
    ResourceTypeKey::new(NGX_SOCKET_TLS_CLIENTCERT_RESOURCE_KEY)
        .expect("ngx.socket.tls.clientcert is a valid resource type key")
}

fn semaphore_param() -> HostParamSchema {
    HostParamSchema::with_passing(
        "sem",
        HostTypeSchema::Resource(semaphore_resource_key()),
        HostParamPassing::Borrow,
    )
}

/// The `fn(bool) -> unknown` timer callback type.
///
/// The callback observes the OpenResty-compatible `premature` flag and its
/// return value is discarded, so the declared result is `unknown`. This is the
/// one deliberate dynamic occurrence on the ngx host surface and the only
/// catalogued callable parameter.
pub fn timer_callback_schema() -> HostTypeSchema {
    HostTypeSchema::Callable {
        params: vec![HostTypeSchema::Bool],
        result: Box::new(HostTypeSchema::Unknown),
    }
}

/// `ngx::timer::{at,every}`: checked seconds plus one owned callback.
fn timer_registration_schema(owner: &str) -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        owner,
        vec![
            HostParamSchema::value("seconds", HostTypeSchema::Number),
            HostParamSchema::with_passing(
                "callback",
                timer_callback_schema(),
                HostParamPassing::TakeOwned,
            ),
        ],
        HostTypeSchema::Bool,
    )
}

fn udp_sock_param() -> HostParamSchema {
    HostParamSchema::with_passing(
        "sock",
        HostTypeSchema::Resource(udp_socket_resource_key()),
        HostParamPassing::Borrow,
    )
}

fn tcp_sock_param() -> HostParamSchema {
    HostParamSchema::with_passing(
        "sock",
        HostTypeSchema::Resource(tcp_socket_resource_key()),
        HostParamPassing::Borrow,
    )
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

fn map_unknown() -> HostTypeSchema {
    HostTypeSchema::Map(Box::new(HostTypeSchema::Unknown))
}

fn decoded_args_result_schema() -> HostTypeSchema {
    HostTypeSchema::named_struct(
        "DecodedArgsResult",
        vec![
            HostStructField::new("args", map_unknown()),
            HostStructField::new("truncated", HostTypeSchema::Bool),
        ],
    )
}

fn request_headers_result_schema() -> HostTypeSchema {
    HostTypeSchema::named_struct(
        "RequestHeadersResult",
        vec![
            HostStructField::new("headers", map_unknown()),
            HostStructField::new("truncated", HostTypeSchema::Bool),
        ],
    )
}

fn request_args_result_schema() -> HostTypeSchema {
    HostTypeSchema::named_struct(
        "RequestArgsResult",
        vec![
            HostStructField::new("args", map_unknown()),
            HostStructField::new("truncated", HostTypeSchema::Bool),
        ],
    )
}

fn optional_string() -> HostTypeSchema {
    HostTypeSchema::Optional(Box::new(HostTypeSchema::String))
}

fn optional_unknown() -> HostTypeSchema {
    HostTypeSchema::Optional(Box::new(HostTypeSchema::Unknown))
}

fn optional_number() -> HostTypeSchema {
    HostTypeSchema::Optional(Box::new(HostTypeSchema::Number))
}

fn shared_get_result_schema() -> HostTypeSchema {
    HostTypeSchema::named_struct(
        "SharedGetResult",
        vec![
            HostStructField::new("value", optional_unknown()),
            HostStructField::new("flags", HostTypeSchema::Int),
            HostStructField::new("stale", HostTypeSchema::Bool),
            HostStructField::new("error", optional_string()),
        ],
    )
}

fn shared_store_result_schema() -> HostTypeSchema {
    HostTypeSchema::named_struct(
        "SharedStoreResult",
        vec![
            HostStructField::new("ok", HostTypeSchema::Bool),
            HostStructField::new("error", optional_string()),
            HostStructField::new("forcible", HostTypeSchema::Bool),
        ],
    )
}

fn shared_safe_store_result_schema() -> HostTypeSchema {
    HostTypeSchema::named_struct(
        "SharedSafeStoreResult",
        vec![
            HostStructField::new("ok", HostTypeSchema::Bool),
            HostStructField::new("error", optional_string()),
        ],
    )
}

fn shared_incr_result_schema() -> HostTypeSchema {
    HostTypeSchema::named_struct(
        "SharedIncrResult",
        vec![
            HostStructField::new("value", optional_number()),
            HostStructField::new("error", optional_string()),
            HostStructField::new("forcible", HostTypeSchema::Bool),
        ],
    )
}

fn shared_number_result_schema() -> HostTypeSchema {
    HostTypeSchema::named_struct(
        "SharedNumberResult",
        vec![
            HostStructField::new("value", optional_number()),
            HostStructField::new("error", optional_string()),
        ],
    )
}

fn dict_handle_param() -> HostParamSchema {
    HostParamSchema::with_passing(
        "dict",
        HostTypeSchema::Resource(crate::shared_dict::shared_dict_resource_key()),
        HostParamPassing::Borrow,
    )
}

fn dict_get_schema(name: &'static str) -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        name,
        vec![
            dict_handle_param(),
            HostParamSchema::value("key", HostTypeSchema::String),
        ],
        shared_get_result_schema(),
    )
}

fn dict_store_schema(name: &'static str) -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        name,
        vec![
            dict_handle_param(),
            HostParamSchema::value("key", HostTypeSchema::String),
            HostParamSchema::value("value", HostTypeSchema::Unknown),
            HostParamSchema::value("exptime", HostTypeSchema::Unknown),
            HostParamSchema::value("flags", HostTypeSchema::Unknown),
        ],
        shared_store_result_schema(),
    )
}

fn dict_safe_store_schema(name: &'static str) -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        name,
        vec![
            dict_handle_param(),
            HostParamSchema::value("key", HostTypeSchema::String),
            HostParamSchema::value("value", HostTypeSchema::Unknown),
            HostParamSchema::value("exptime", HostTypeSchema::Unknown),
            HostParamSchema::value("flags", HostTypeSchema::Unknown),
        ],
        shared_safe_store_result_schema(),
    )
}

fn host_function_contract(name: &str) -> HostFunctionSchema {
    match name {
        NGX_SLEEP_NAME => sleep_schema(),
        NGX_SLEEP_MS_NAME => sleep_ms_schema(),
        NGX_SEMAPHORE_NEW_NAME => HostFunctionSchema::with_return(
            NGX_SEMAPHORE_NEW_NAME,
            vec![HostParamSchema::value("count", HostTypeSchema::Int)],
            HostTypeSchema::Resource(semaphore_resource_key()),
        ),
        NGX_SEMAPHORE_WAIT_NAME => HostFunctionSchema::with_return(
            NGX_SEMAPHORE_WAIT_NAME,
            vec![
                semaphore_param(),
                HostParamSchema::value("timeout", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Bool,
        ),
        NGX_SEMAPHORE_POST_NAME => HostFunctionSchema::with_return(
            NGX_SEMAPHORE_POST_NAME,
            vec![
                semaphore_param(),
                HostParamSchema::value("count", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Null,
        ),
        NGX_SEMAPHORE_COUNT_NAME => HostFunctionSchema::with_return(
            NGX_SEMAPHORE_COUNT_NAME,
            vec![semaphore_param()],
            HostTypeSchema::Int,
        ),
        NGX_TIMER_AT_NAME | NGX_TIMER_EVERY_NAME => timer_registration_schema(name),
        NGX_TIMER_PENDING_COUNT_NAME | NGX_TIMER_RUNNING_COUNT_NAME => {
            HostFunctionSchema::with_return(name, Vec::new(), HostTypeSchema::Int)
        }
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
            decoded_args_result_schema(),
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
            request_headers_result_schema(),
        ),
        NGX_REQ_GET_URI_ARGS_NAME => HostFunctionSchema::with_return(
            NGX_REQ_GET_URI_ARGS_NAME,
            vec![HostParamSchema::value("max_args", HostTypeSchema::Unknown)],
            request_args_result_schema(),
        ),
        NGX_REQ_GET_POST_ARGS_NAME => HostFunctionSchema::with_return(
            NGX_REQ_GET_POST_ARGS_NAME,
            vec![HostParamSchema::value("max_args", HostTypeSchema::Unknown)],
            request_args_result_schema(),
        ),
        NGX_REQ_READ_BODY_NAME => nullary(NGX_REQ_READ_BODY_NAME, HostTypeSchema::Null),
        NGX_REQ_DISCARD_BODY_NAME => nullary(NGX_REQ_DISCARD_BODY_NAME, HostTypeSchema::Null),
        NGX_REQ_GET_BODY_DATA_NAME => nullary(
            NGX_REQ_GET_BODY_DATA_NAME,
            HostTypeSchema::Optional(Box::new(HostTypeSchema::String)),
        ),
        NGX_REQ_GET_BODY_FILE_NAME => nullary(
            NGX_REQ_GET_BODY_FILE_NAME,
            HostTypeSchema::Optional(Box::new(HostTypeSchema::String)),
        ),
        NGX_REQ_SET_BODY_DATA_NAME => HostFunctionSchema::with_return(
            NGX_REQ_SET_BODY_DATA_NAME,
            vec![HostParamSchema::value("data", HostTypeSchema::String)],
            HostTypeSchema::Null,
        ),
        NGX_REQ_SET_BODY_FILE_NAME => HostFunctionSchema::with_return(
            NGX_REQ_SET_BODY_FILE_NAME,
            vec![
                HostParamSchema::value("path", HostTypeSchema::String),
                HostParamSchema::value("auto_clean", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Null,
        ),
        NGX_REQ_INIT_BODY_NAME => HostFunctionSchema::with_return(
            NGX_REQ_INIT_BODY_NAME,
            vec![HostParamSchema::value(
                "buffer_size",
                HostTypeSchema::Unknown,
            )],
            HostTypeSchema::Null,
        ),
        NGX_REQ_APPEND_BODY_NAME => HostFunctionSchema::with_return(
            NGX_REQ_APPEND_BODY_NAME,
            vec![HostParamSchema::value("data", HostTypeSchema::String)],
            HostTypeSchema::Null,
        ),
        NGX_REQ_FINISH_BODY_NAME => nullary(NGX_REQ_FINISH_BODY_NAME, HostTypeSchema::Null),
        NGX_REQ_IS_INTERNAL_NAME => nullary(NGX_REQ_IS_INTERNAL_NAME, HostTypeSchema::Bool),
        NGX_REQ_START_TIME_NAME => nullary(NGX_REQ_START_TIME_NAME, HostTypeSchema::Float),
        NGX_IS_SUBREQUEST_NAME => nullary(NGX_IS_SUBREQUEST_NAME, HostTypeSchema::Bool),
        NGX_HEADERS_SENT_NAME => nullary(NGX_HEADERS_SENT_NAME, HostTypeSchema::Bool),
        NGX_VAR_GET_NAME => HostFunctionSchema::with_return(
            NGX_VAR_GET_NAME,
            vec![HostParamSchema::value("name", HostTypeSchema::String)],
            HostTypeSchema::Optional(Box::new(HostTypeSchema::String)),
        ),
        NGX_REQ_SET_METHOD_NAME => HostFunctionSchema::with_return(
            NGX_REQ_SET_METHOD_NAME,
            vec![HostParamSchema::value("method_id", HostTypeSchema::Int)],
            HostTypeSchema::Null,
        ),
        NGX_REQ_SET_URI_NAME => HostFunctionSchema::with_return(
            NGX_REQ_SET_URI_NAME,
            vec![
                HostParamSchema::value("uri", HostTypeSchema::String),
                HostParamSchema::value("jump", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Null,
        ),
        NGX_REQ_SET_URI_ARGS_NAME => HostFunctionSchema::with_return(
            NGX_REQ_SET_URI_ARGS_NAME,
            vec![HostParamSchema::value("args", HostTypeSchema::Unknown)],
            HostTypeSchema::Null,
        ),
        NGX_REQ_SET_HEADER_NAME => HostFunctionSchema::with_return(
            NGX_REQ_SET_HEADER_NAME,
            vec![
                HostParamSchema::value("name", HostTypeSchema::String),
                HostParamSchema::value("value", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Null,
        ),
        NGX_REQ_CLEAR_HEADER_NAME => HostFunctionSchema::with_return(
            NGX_REQ_CLEAR_HEADER_NAME,
            vec![HostParamSchema::value("name", HostTypeSchema::String)],
            HostTypeSchema::Null,
        ),
        NGX_VAR_SET_NAME => HostFunctionSchema::with_return(
            NGX_VAR_SET_NAME,
            vec![
                HostParamSchema::value("name", HostTypeSchema::String),
                HostParamSchema::value("value", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Null,
        ),
        NGX_STATUS_GET_NAME => nullary(NGX_STATUS_GET_NAME, HostTypeSchema::Int),
        NGX_STATUS_SET_NAME => HostFunctionSchema::with_return(
            NGX_STATUS_SET_NAME,
            vec![HostParamSchema::value("status", HostTypeSchema::Int)],
            HostTypeSchema::Null,
        ),
        NGX_HEADER_GET_NAME => HostFunctionSchema::with_return(
            NGX_HEADER_GET_NAME,
            vec![HostParamSchema::value("name", HostTypeSchema::String)],
            HostTypeSchema::Unknown,
        ),
        NGX_HEADER_SET_NAME => HostFunctionSchema::with_return(
            NGX_HEADER_SET_NAME,
            vec![
                HostParamSchema::value("name", HostTypeSchema::String),
                HostParamSchema::value("value", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Null,
        ),
        NGX_HEADER_SET_ALL_NAME => HostFunctionSchema::with_return(
            NGX_HEADER_SET_ALL_NAME,
            vec![HostParamSchema::value("headers", HostTypeSchema::Unknown)],
            HostTypeSchema::Null,
        ),
        NGX_HEADER_CLEAR_NAME => nullary(NGX_HEADER_CLEAR_NAME, HostTypeSchema::Null),
        NGX_EXIT_NAME => HostFunctionSchema::with_return(
            NGX_EXIT_NAME,
            vec![HostParamSchema::value("status", HostTypeSchema::Int)],
            HostTypeSchema::Null,
        ),
        NGX_REDIRECT_NAME => HostFunctionSchema::with_return(
            NGX_REDIRECT_NAME,
            vec![
                HostParamSchema::value("uri", HostTypeSchema::String),
                HostParamSchema::value("status", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Null,
        ),
        NGX_CTX_GET_NAME => HostFunctionSchema::with_return(
            NGX_CTX_GET_NAME,
            vec![HostParamSchema::value("key", HostTypeSchema::String)],
            HostTypeSchema::Unknown,
        ),
        NGX_CTX_SET_NAME => HostFunctionSchema::with_return(
            NGX_CTX_SET_NAME,
            vec![
                HostParamSchema::value("key", HostTypeSchema::String),
                HostParamSchema::value("value", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Null,
        ),
        NGX_CTX_DELETE_NAME => HostFunctionSchema::with_return(
            NGX_CTX_DELETE_NAME,
            vec![HostParamSchema::value("key", HostTypeSchema::String)],
            HostTypeSchema::Unknown,
        ),
        NGX_CTX_CLEAR_NAME => nullary(NGX_CTX_CLEAR_NAME, HostTypeSchema::Null),
        NGX_SHARED_DICT_NAME => HostFunctionSchema::with_return(
            NGX_SHARED_DICT_NAME,
            vec![HostParamSchema::value("name", HostTypeSchema::String)],
            HostTypeSchema::Resource(crate::shared_dict::shared_dict_resource_key()),
        ),
        NGX_SHARED_GET_NAME => dict_get_schema(NGX_SHARED_GET_NAME),
        NGX_SHARED_GET_STALE_NAME => dict_get_schema(NGX_SHARED_GET_STALE_NAME),
        NGX_SHARED_SET_NAME => dict_store_schema(NGX_SHARED_SET_NAME),
        NGX_SHARED_SAFE_SET_NAME => dict_safe_store_schema(NGX_SHARED_SAFE_SET_NAME),
        NGX_SHARED_ADD_NAME => dict_store_schema(NGX_SHARED_ADD_NAME),
        NGX_SHARED_SAFE_ADD_NAME => dict_safe_store_schema(NGX_SHARED_SAFE_ADD_NAME),
        NGX_SHARED_REPLACE_NAME => dict_store_schema(NGX_SHARED_REPLACE_NAME),
        NGX_SHARED_DELETE_NAME => HostFunctionSchema::with_return(
            NGX_SHARED_DELETE_NAME,
            vec![
                dict_handle_param(),
                HostParamSchema::value("key", HostTypeSchema::String),
            ],
            HostTypeSchema::Null,
        ),
        NGX_SHARED_INCR_NAME => HostFunctionSchema::with_return(
            NGX_SHARED_INCR_NAME,
            vec![
                dict_handle_param(),
                HostParamSchema::value("key", HostTypeSchema::String),
                HostParamSchema::value("value", HostTypeSchema::Unknown),
                HostParamSchema::value("init", HostTypeSchema::Unknown),
                HostParamSchema::value("init_ttl", HostTypeSchema::Unknown),
            ],
            shared_incr_result_schema(),
        ),
        NGX_SHARED_TTL_NAME => HostFunctionSchema::with_return(
            NGX_SHARED_TTL_NAME,
            vec![
                dict_handle_param(),
                HostParamSchema::value("key", HostTypeSchema::String),
            ],
            shared_number_result_schema(),
        ),
        NGX_SHARED_EXPIRE_NAME => HostFunctionSchema::with_return(
            NGX_SHARED_EXPIRE_NAME,
            vec![
                dict_handle_param(),
                HostParamSchema::value("key", HostTypeSchema::String),
                HostParamSchema::value("exptime", HostTypeSchema::Unknown),
            ],
            shared_safe_store_result_schema(),
        ),
        NGX_SHARED_CAPACITY_NAME => HostFunctionSchema::with_return(
            NGX_SHARED_CAPACITY_NAME,
            vec![dict_handle_param()],
            HostTypeSchema::Int,
        ),
        NGX_SHARED_FREE_SPACE_NAME => HostFunctionSchema::with_return(
            NGX_SHARED_FREE_SPACE_NAME,
            vec![dict_handle_param()],
            HostTypeSchema::Int,
        ),
        NGX_SHARED_LPUSH_NAME => HostFunctionSchema::with_return(
            NGX_SHARED_LPUSH_NAME,
            vec![
                dict_handle_param(),
                HostParamSchema::value("key", HostTypeSchema::String),
                HostParamSchema::value("value", HostTypeSchema::Unknown),
            ],
            shared_number_result_schema(),
        ),
        NGX_SHARED_RPUSH_NAME => HostFunctionSchema::with_return(
            NGX_SHARED_RPUSH_NAME,
            vec![
                dict_handle_param(),
                HostParamSchema::value("key", HostTypeSchema::String),
                HostParamSchema::value("value", HostTypeSchema::Unknown),
            ],
            shared_number_result_schema(),
        ),
        NGX_SHARED_LPOP_NAME => dict_get_schema(NGX_SHARED_LPOP_NAME),
        NGX_SHARED_RPOP_NAME => dict_get_schema(NGX_SHARED_RPOP_NAME),
        NGX_SHARED_LLEN_NAME => HostFunctionSchema::with_return(
            NGX_SHARED_LLEN_NAME,
            vec![
                dict_handle_param(),
                HostParamSchema::value("key", HostTypeSchema::String),
            ],
            shared_number_result_schema(),
        ),
        NGX_SHARED_FLUSH_ALL_NAME => HostFunctionSchema::with_return(
            NGX_SHARED_FLUSH_ALL_NAME,
            vec![dict_handle_param()],
            HostTypeSchema::Null,
        ),
        NGX_SHARED_FLUSH_EXPIRED_NAME => HostFunctionSchema::with_return(
            NGX_SHARED_FLUSH_EXPIRED_NAME,
            vec![
                dict_handle_param(),
                HostParamSchema::value("max", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Int,
        ),
        NGX_SHARED_GET_KEYS_NAME => HostFunctionSchema::with_return(
            NGX_SHARED_GET_KEYS_NAME,
            vec![
                dict_handle_param(),
                HostParamSchema::value("max", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Array(Box::new(HostTypeSchema::String)),
        ),
        NGX_SOCKET_UDP_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_UDP_NAME,
            vec![],
            HostTypeSchema::Resource(udp_socket_resource_key()),
        ),
        NGX_SOCKET_UDP_BIND_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_UDP_BIND_NAME,
            vec![
                udp_sock_param(),
                HostParamSchema::value("host", HostTypeSchema::Unknown),
                HostParamSchema::value("port", HostTypeSchema::Int),
            ],
            HostTypeSchema::Null,
        ),
        NGX_SOCKET_UDP_SETPEERNAME_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_UDP_SETPEERNAME_NAME,
            vec![
                udp_sock_param(),
                HostParamSchema::value("host", HostTypeSchema::String),
                HostParamSchema::value("port", HostTypeSchema::Int),
            ],
            HostTypeSchema::Null,
        ),
        NGX_SOCKET_UDP_SEND_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_UDP_SEND_NAME,
            vec![
                udp_sock_param(),
                HostParamSchema::value("data", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Null,
        ),
        NGX_SOCKET_UDP_RECEIVE_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_UDP_RECEIVE_NAME,
            vec![
                udp_sock_param(),
                HostParamSchema::value("size", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Bytes,
        ),
        NGX_SOCKET_UDP_CLOSE_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_UDP_CLOSE_NAME,
            vec![udp_sock_param()],
            HostTypeSchema::Null,
        ),
        NGX_SOCKET_UDP_SETTIMEOUT_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_UDP_SETTIMEOUT_NAME,
            vec![
                udp_sock_param(),
                HostParamSchema::value("ms", HostTypeSchema::Int),
            ],
            HostTypeSchema::Null,
        ),
        NGX_SOCKET_TCP_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_NAME,
            vec![],
            HostTypeSchema::Resource(tcp_socket_resource_key()),
        ),
        NGX_SOCKET_TCP_CONNECT_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_CONNECT_NAME,
            vec![
                tcp_sock_param(),
                HostParamSchema::value("host", HostTypeSchema::String),
                HostParamSchema::value("port", HostTypeSchema::Int),
            ],
            HostTypeSchema::Null,
        ),
        NGX_SOCKET_CONNECT_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_CONNECT_NAME,
            vec![
                HostParamSchema::value("host", HostTypeSchema::String),
                HostParamSchema::value("port", HostTypeSchema::Int),
            ],
            HostTypeSchema::Resource(tcp_socket_resource_key()),
        ),
        NGX_SOCKET_TCP_SEND_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_SEND_NAME,
            vec![
                tcp_sock_param(),
                HostParamSchema::value("data", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Null,
        ),
        NGX_SOCKET_TCP_RECEIVE_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_RECEIVE_NAME,
            vec![
                tcp_sock_param(),
                HostParamSchema::value("mode", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Bytes,
        ),
        NGX_SOCKET_TCP_RECEIVEANY_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_RECEIVEANY_NAME,
            vec![
                tcp_sock_param(),
                HostParamSchema::value("max", HostTypeSchema::Int),
            ],
            HostTypeSchema::Bytes,
        ),
        NGX_SOCKET_TCP_RECEIVEUNTIL_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_RECEIVEUNTIL_NAME,
            vec![
                tcp_sock_param(),
                HostParamSchema::value("delimiter", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Bytes,
        ),
        NGX_SOCKET_TCP_CLOSE_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_CLOSE_NAME,
            vec![tcp_sock_param()],
            HostTypeSchema::Null,
        ),
        NGX_SOCKET_TCP_SETTIMEOUT_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_SETTIMEOUT_NAME,
            vec![
                tcp_sock_param(),
                HostParamSchema::value("ms", HostTypeSchema::Int),
            ],
            HostTypeSchema::Null,
        ),
        NGX_SOCKET_TCP_SETKEEPALIVE_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_SETKEEPALIVE_NAME,
            vec![
                tcp_sock_param(),
                HostParamSchema::value("timeout", HostTypeSchema::Unknown),
                HostParamSchema::value("pool_size", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Null,
        ),
        NGX_SOCKET_TCP_GETREUSEDTIMES_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_GETREUSEDTIMES_NAME,
            vec![tcp_sock_param()],
            HostTypeSchema::Int,
        ),
        NGX_SOCKET_TCP_SSLHANDSHAKE_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_SSLHANDSHAKE_NAME,
            vec![
                tcp_sock_param(),
                HostParamSchema::value("reused_session", HostTypeSchema::Unknown),
                HostParamSchema::value("server_name", HostTypeSchema::Unknown),
                HostParamSchema::value("ssl_verify", HostTypeSchema::Unknown),
                HostParamSchema::value("send_status_req", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Null,
        ),
        NGX_SOCKET_TCP_SETTRUSTEDSTORE_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_SETTRUSTEDSTORE_NAME,
            vec![
                tcp_sock_param(),
                HostParamSchema::with_passing(
                    "store",
                    HostTypeSchema::Resource(tls_trustedstore_resource_key()),
                    HostParamPassing::Borrow,
                ),
            ],
            HostTypeSchema::Null,
        ),
        NGX_SOCKET_TCP_SETCLIENTCERT_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_SETCLIENTCERT_NAME,
            vec![
                tcp_sock_param(),
                HostParamSchema::with_passing(
                    "cert",
                    HostTypeSchema::Resource(tls_clientcert_resource_key()),
                    HostParamPassing::Borrow,
                ),
            ],
            HostTypeSchema::Null,
        ),
        NGX_SOCKET_TCP_TRUSTEDSTORE_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_TRUSTEDSTORE_NAME,
            vec![HostParamSchema::value("pem", HostTypeSchema::Unknown)],
            HostTypeSchema::Resource(tls_trustedstore_resource_key()),
        ),
        NGX_SOCKET_TCP_CLIENTCERT_NAME => HostFunctionSchema::with_return(
            NGX_SOCKET_TCP_CLIENTCERT_NAME,
            vec![
                HostParamSchema::value("cert", HostTypeSchema::Unknown),
                HostParamSchema::value("pkey", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Resource(tls_clientcert_resource_key()),
        ),
        NGX_SEND_HEADERS_NAME => nullary(NGX_SEND_HEADERS_NAME, HostTypeSchema::Null),
        NGX_PRINT_NAME => HostFunctionSchema::with_return(
            NGX_PRINT_NAME,
            vec![HostParamSchema::value("data", HostTypeSchema::Unknown)],
            HostTypeSchema::Null,
        ),
        NGX_SAY_NAME => HostFunctionSchema::with_return(
            NGX_SAY_NAME,
            vec![HostParamSchema::value("data", HostTypeSchema::Unknown)],
            HostTypeSchema::Null,
        ),
        NGX_FLUSH_NAME => HostFunctionSchema::with_return(
            NGX_FLUSH_NAME,
            vec![HostParamSchema::value("wait", HostTypeSchema::Bool)],
            HostTypeSchema::Null,
        ),
        NGX_EOF_NAME => nullary(NGX_EOF_NAME, HostTypeSchema::Null),
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

pub(crate) fn ngx_req_read_body_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::read_body")
}

pub(crate) fn ngx_req_discard_body_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::discard_body")
}

pub(crate) fn ngx_req_get_body_data_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::get_body_data")
}

pub(crate) fn ngx_req_get_body_file_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::get_body_file")
}

pub(crate) fn ngx_req_set_body_data_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::set_body_data")
}

pub(crate) fn ngx_req_set_body_file_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::set_body_file")
}

pub(crate) fn ngx_req_init_body_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::init_body")
}

pub(crate) fn ngx_req_append_body_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::append_body")
}

pub(crate) fn ngx_req_finish_body_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::finish_body")
}

pub(crate) fn ngx_escape_uri_contract() -> HostFunctionSchema {
    host_function_contract("ngx::escape_uri")
}

pub(crate) fn ngx_unescape_uri_contract() -> HostFunctionSchema {
    host_function_contract("ngx::unescape_uri")
}

pub(crate) fn ngx_encode_args_contract() -> HostFunctionSchema {
    host_function_contract("ngx::encode_args")
}

pub(crate) fn ngx_decode_args_contract() -> HostFunctionSchema {
    host_function_contract("ngx::decode_args")
}

pub(crate) fn ngx_encode_base64_contract() -> HostFunctionSchema {
    host_function_contract("ngx::encode_base64")
}

pub(crate) fn ngx_decode_base64_contract() -> HostFunctionSchema {
    host_function_contract("ngx::decode_base64")
}

pub(crate) fn ngx_decode_base64mime_contract() -> HostFunctionSchema {
    host_function_contract("ngx::decode_base64mime")
}

pub(crate) fn ngx_crc32_short_contract() -> HostFunctionSchema {
    host_function_contract("ngx::crc32_short")
}

pub(crate) fn ngx_crc32_long_contract() -> HostFunctionSchema {
    host_function_contract("ngx::crc32_long")
}

pub(crate) fn ngx_hmac_sha1_contract() -> HostFunctionSchema {
    host_function_contract("ngx::hmac_sha1")
}

pub(crate) fn ngx_md5_contract() -> HostFunctionSchema {
    host_function_contract("ngx::md5")
}

pub(crate) fn ngx_md5_bin_contract() -> HostFunctionSchema {
    host_function_contract("ngx::md5_bin")
}

pub(crate) fn ngx_sha1_bin_contract() -> HostFunctionSchema {
    host_function_contract("ngx::sha1_bin")
}

pub(crate) fn ngx_quote_sql_str_contract() -> HostFunctionSchema {
    host_function_contract("ngx::quote_sql_str")
}

pub(crate) fn ngx_log_contract() -> HostFunctionSchema {
    host_function_contract("ngx::log")
}

pub(crate) fn ngx_core_ok_contract() -> HostFunctionSchema {
    host_function_contract("ngx::core::ok")
}

pub(crate) fn ngx_core_error_contract() -> HostFunctionSchema {
    host_function_contract("ngx::core::error")
}

pub(crate) fn ngx_core_again_contract() -> HostFunctionSchema {
    host_function_contract("ngx::core::again")
}

pub(crate) fn ngx_core_done_contract() -> HostFunctionSchema {
    host_function_contract("ngx::core::done")
}

pub(crate) fn ngx_core_declined_contract() -> HostFunctionSchema {
    host_function_contract("ngx::core::declined")
}

pub(crate) fn ngx_http_get_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::get")
}

pub(crate) fn ngx_http_head_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::head")
}

pub(crate) fn ngx_http_put_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::put")
}

pub(crate) fn ngx_http_post_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::post")
}

pub(crate) fn ngx_http_delete_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::delete")
}

pub(crate) fn ngx_http_options_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::options")
}

pub(crate) fn ngx_http_mkcol_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::mkcol")
}

pub(crate) fn ngx_http_copy_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::copy")
}

pub(crate) fn ngx_http_move_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::move")
}

pub(crate) fn ngx_http_propfind_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::propfind")
}

pub(crate) fn ngx_http_proppatch_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::proppatch")
}

pub(crate) fn ngx_http_lock_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::lock")
}

pub(crate) fn ngx_http_unlock_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::unlock")
}

pub(crate) fn ngx_http_patch_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::patch")
}

pub(crate) fn ngx_http_trace_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::trace")
}

pub(crate) fn ngx_http_status_continue_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_continue")
}

pub(crate) fn ngx_http_status_switching_protocols_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_switching_protocols")
}

pub(crate) fn ngx_http_status_ok_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_ok")
}

pub(crate) fn ngx_http_status_created_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_created")
}

pub(crate) fn ngx_http_status_accepted_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_accepted")
}

pub(crate) fn ngx_http_status_no_content_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_no_content")
}

pub(crate) fn ngx_http_status_partial_content_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_partial_content")
}

pub(crate) fn ngx_http_status_special_response_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_special_response")
}

pub(crate) fn ngx_http_status_moved_permanently_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_moved_permanently")
}

pub(crate) fn ngx_http_status_moved_temporarily_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_moved_temporarily")
}

pub(crate) fn ngx_http_status_see_other_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_see_other")
}

pub(crate) fn ngx_http_status_not_modified_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_not_modified")
}

pub(crate) fn ngx_http_status_temporary_redirect_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_temporary_redirect")
}

pub(crate) fn ngx_http_status_permanent_redirect_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_permanent_redirect")
}

pub(crate) fn ngx_http_status_bad_request_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_bad_request")
}

pub(crate) fn ngx_http_status_unauthorized_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_unauthorized")
}

pub(crate) fn ngx_http_status_payment_required_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_payment_required")
}

pub(crate) fn ngx_http_status_forbidden_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_forbidden")
}

pub(crate) fn ngx_http_status_not_found_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_not_found")
}

pub(crate) fn ngx_http_status_not_allowed_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_not_allowed")
}

pub(crate) fn ngx_http_status_not_acceptable_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_not_acceptable")
}

pub(crate) fn ngx_http_status_request_timeout_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_request_timeout")
}

pub(crate) fn ngx_http_status_conflict_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_conflict")
}

pub(crate) fn ngx_http_status_gone_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_gone")
}

pub(crate) fn ngx_http_status_upgrade_required_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_upgrade_required")
}

pub(crate) fn ngx_http_status_too_many_requests_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_too_many_requests")
}

pub(crate) fn ngx_http_status_illegal_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_illegal")
}

pub(crate) fn ngx_http_status_close_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_close")
}

pub(crate) fn ngx_http_status_internal_server_error_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_internal_server_error")
}

pub(crate) fn ngx_http_status_not_implemented_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_not_implemented")
}

pub(crate) fn ngx_http_status_bad_gateway_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_bad_gateway")
}

pub(crate) fn ngx_http_status_service_unavailable_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_service_unavailable")
}

pub(crate) fn ngx_http_status_gateway_timeout_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_gateway_timeout")
}

pub(crate) fn ngx_http_status_version_not_supported_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_version_not_supported")
}

pub(crate) fn ngx_http_status_insufficient_storage_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http::status_insufficient_storage")
}

pub(crate) fn ngx_log_level_stderr_contract() -> HostFunctionSchema {
    host_function_contract("ngx::log_level::stderr")
}

pub(crate) fn ngx_log_level_emerg_contract() -> HostFunctionSchema {
    host_function_contract("ngx::log_level::emerg")
}

pub(crate) fn ngx_log_level_alert_contract() -> HostFunctionSchema {
    host_function_contract("ngx::log_level::alert")
}

pub(crate) fn ngx_log_level_crit_contract() -> HostFunctionSchema {
    host_function_contract("ngx::log_level::crit")
}

pub(crate) fn ngx_log_level_error_contract() -> HostFunctionSchema {
    host_function_contract("ngx::log_level::error")
}

pub(crate) fn ngx_log_level_warn_contract() -> HostFunctionSchema {
    host_function_contract("ngx::log_level::warn")
}

pub(crate) fn ngx_log_level_notice_contract() -> HostFunctionSchema {
    host_function_contract("ngx::log_level::notice")
}

pub(crate) fn ngx_log_level_info_contract() -> HostFunctionSchema {
    host_function_contract("ngx::log_level::info")
}

pub(crate) fn ngx_log_level_debug_contract() -> HostFunctionSchema {
    host_function_contract("ngx::log_level::debug")
}

pub(crate) fn ngx_core_null_value_contract() -> HostFunctionSchema {
    host_function_contract("ngx::core::null_value")
}

pub(crate) fn ngx_get_phase_contract() -> HostFunctionSchema {
    host_function_contract("ngx::get_phase")
}

pub(crate) fn ngx_config_subsystem_contract() -> HostFunctionSchema {
    host_function_contract("ngx::config::subsystem")
}

pub(crate) fn ngx_config_debug_contract() -> HostFunctionSchema {
    host_function_contract("ngx::config::debug")
}

pub(crate) fn ngx_config_prefix_contract() -> HostFunctionSchema {
    host_function_contract("ngx::config::prefix")
}

pub(crate) fn ngx_config_nginx_version_contract() -> HostFunctionSchema {
    host_function_contract("ngx::config::nginx_version")
}

pub(crate) fn ngx_config_nginx_configure_contract() -> HostFunctionSchema {
    host_function_contract("ngx::config::nginx_configure")
}

pub(crate) fn ngx_config_ngx_rustscript_version_contract() -> HostFunctionSchema {
    host_function_contract("ngx::config::ngx_rustscript_version")
}

pub(crate) fn ngx_worker_exiting_contract() -> HostFunctionSchema {
    host_function_contract("ngx::worker::exiting")
}

pub(crate) fn ngx_worker_pid_contract() -> HostFunctionSchema {
    host_function_contract("ngx::worker::pid")
}

pub(crate) fn ngx_worker_pids_contract() -> HostFunctionSchema {
    host_function_contract("ngx::worker::pids")
}

pub(crate) fn ngx_worker_count_contract() -> HostFunctionSchema {
    host_function_contract("ngx::worker::count")
}

pub(crate) fn ngx_worker_id_contract() -> HostFunctionSchema {
    host_function_contract("ngx::worker::id")
}

pub(crate) fn ngx_sleep_contract() -> HostFunctionSchema {
    host_function_contract("ngx::sleep")
}

pub(crate) fn ngx_sleep_ms_contract() -> HostFunctionSchema {
    host_function_contract("ngx::sleep_ms")
}

pub(crate) fn ngx_send_headers_contract() -> HostFunctionSchema {
    host_function_contract("ngx::send_headers")
}

pub(crate) fn ngx_print_contract() -> HostFunctionSchema {
    host_function_contract("ngx::print")
}

pub(crate) fn ngx_say_contract() -> HostFunctionSchema {
    host_function_contract("ngx::say")
}

pub(crate) fn ngx_flush_contract() -> HostFunctionSchema {
    host_function_contract("ngx::flush")
}

pub(crate) fn ngx_eof_contract() -> HostFunctionSchema {
    host_function_contract("ngx::eof")
}

pub(crate) fn ngx_req_get_method_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::get_method")
}

pub(crate) fn ngx_req_http_version_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::http_version")
}

pub(crate) fn ngx_req_raw_header_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::raw_header")
}

pub(crate) fn ngx_req_get_headers_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::get_headers")
}

pub(crate) fn ngx_req_get_uri_args_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::get_uri_args")
}

pub(crate) fn ngx_req_get_post_args_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::get_post_args")
}

pub(crate) fn ngx_req_is_internal_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::is_internal")
}

pub(crate) fn ngx_req_start_time_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::start_time")
}

pub(crate) fn ngx_is_subrequest_contract() -> HostFunctionSchema {
    host_function_contract("ngx::is_subrequest")
}

pub(crate) fn ngx_headers_sent_contract() -> HostFunctionSchema {
    host_function_contract("ngx::headers_sent")
}

pub(crate) fn ngx_req_set_method_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::set_method")
}

pub(crate) fn ngx_req_set_uri_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::set_uri")
}

pub(crate) fn ngx_req_set_uri_args_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::set_uri_args")
}

pub(crate) fn ngx_req_set_header_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::set_header")
}

pub(crate) fn ngx_req_clear_header_contract() -> HostFunctionSchema {
    host_function_contract("ngx::req::clear_header")
}

pub(crate) fn ngx_ctx_get_contract() -> HostFunctionSchema {
    host_function_contract("ngx::ctx::get")
}

pub(crate) fn ngx_ctx_set_contract() -> HostFunctionSchema {
    host_function_contract("ngx::ctx::set")
}

pub(crate) fn ngx_ctx_delete_contract() -> HostFunctionSchema {
    host_function_contract("ngx::ctx::delete")
}

pub(crate) fn ngx_ctx_clear_contract() -> HostFunctionSchema {
    host_function_contract("ngx::ctx::clear")
}

pub(crate) fn ngx_status_get_contract() -> HostFunctionSchema {
    host_function_contract("ngx::status::get")
}

pub(crate) fn ngx_status_set_contract() -> HostFunctionSchema {
    host_function_contract("ngx::status::set")
}

pub(crate) fn ngx_header_get_contract() -> HostFunctionSchema {
    host_function_contract("ngx::header::get")
}

pub(crate) fn ngx_header_set_contract() -> HostFunctionSchema {
    host_function_contract("ngx::header::set")
}

pub(crate) fn ngx_header_set_all_contract() -> HostFunctionSchema {
    host_function_contract("ngx::header::set_all")
}

pub(crate) fn ngx_header_clear_contract() -> HostFunctionSchema {
    host_function_contract("ngx::header::clear")
}

pub(crate) fn ngx_exit_contract() -> HostFunctionSchema {
    host_function_contract("ngx::exit")
}

pub(crate) fn ngx_redirect_contract() -> HostFunctionSchema {
    host_function_contract("ngx::redirect")
}

pub(crate) fn ngx_semaphore_new_contract() -> HostFunctionSchema {
    host_function_contract("ngx::semaphore::new")
}

pub(crate) fn ngx_semaphore_wait_contract() -> HostFunctionSchema {
    host_function_contract("ngx::semaphore::wait")
}

pub(crate) fn ngx_semaphore_post_contract() -> HostFunctionSchema {
    host_function_contract("ngx::semaphore::post")
}

pub(crate) fn ngx_semaphore_count_contract() -> HostFunctionSchema {
    host_function_contract("ngx::semaphore::count")
}

pub(crate) fn ngx_shared_dict_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::dict")
}

pub(crate) fn ngx_shared_get_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::get")
}

pub(crate) fn ngx_shared_get_stale_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::get_stale")
}

pub(crate) fn ngx_shared_set_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::set")
}

pub(crate) fn ngx_shared_safe_set_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::safe_set")
}

pub(crate) fn ngx_shared_add_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::add")
}

pub(crate) fn ngx_shared_safe_add_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::safe_add")
}

pub(crate) fn ngx_shared_replace_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::replace")
}

pub(crate) fn ngx_shared_delete_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::delete")
}

pub(crate) fn ngx_shared_incr_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::incr")
}

pub(crate) fn ngx_shared_ttl_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::ttl")
}

pub(crate) fn ngx_shared_expire_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::expire")
}

pub(crate) fn ngx_shared_capacity_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::capacity")
}

pub(crate) fn ngx_shared_free_space_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::free_space")
}

pub(crate) fn ngx_shared_lpush_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::lpush")
}

pub(crate) fn ngx_shared_rpush_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::rpush")
}

pub(crate) fn ngx_shared_lpop_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::lpop")
}

pub(crate) fn ngx_shared_rpop_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::rpop")
}

pub(crate) fn ngx_shared_llen_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::llen")
}

pub(crate) fn ngx_shared_flush_all_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::flush_all")
}

pub(crate) fn ngx_shared_flush_expired_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::flush_expired")
}

pub(crate) fn ngx_shared_get_keys_contract() -> HostFunctionSchema {
    host_function_contract("ngx::shared::get_keys")
}

pub(crate) fn ngx_socket_udp_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::udp")
}

pub(crate) fn ngx_socket_udp_bind_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::udp::bind")
}

pub(crate) fn ngx_socket_udp_setpeername_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::udp::setpeername")
}

pub(crate) fn ngx_socket_udp_send_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::udp::send")
}

pub(crate) fn ngx_socket_udp_receive_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::udp::receive")
}

pub(crate) fn ngx_socket_udp_close_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::udp::close")
}

pub(crate) fn ngx_socket_udp_settimeout_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::udp::settimeout")
}

pub(crate) fn ngx_socket_tcp_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp")
}

pub(crate) fn ngx_socket_tcp_connect_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::connect")
}

pub(crate) fn ngx_socket_connect_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::connect")
}

pub(crate) fn ngx_socket_tcp_send_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::send")
}

pub(crate) fn ngx_socket_tcp_receive_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::receive")
}

pub(crate) fn ngx_socket_tcp_receiveany_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::receiveany")
}

pub(crate) fn ngx_socket_tcp_receiveuntil_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::receiveuntil")
}

pub(crate) fn ngx_socket_tcp_close_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::close")
}

pub(crate) fn ngx_socket_tcp_settimeout_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::settimeout")
}

pub(crate) fn ngx_socket_tcp_setkeepalive_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::setkeepalive")
}

pub(crate) fn ngx_socket_tcp_getreusedtimes_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::getreusedtimes")
}

pub(crate) fn ngx_socket_tcp_sslhandshake_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::sslhandshake")
}

pub(crate) fn ngx_socket_tcp_settrustedstore_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::settrustedstore")
}

pub(crate) fn ngx_socket_tcp_setclientcert_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::setclientcert")
}

pub(crate) fn ngx_socket_tcp_trustedstore_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::trustedstore")
}

pub(crate) fn ngx_socket_tcp_clientcert_contract() -> HostFunctionSchema {
    host_function_contract("ngx::socket::tcp::clientcert")
}

pub(crate) fn ngx_today_contract() -> HostFunctionSchema {
    host_function_contract("ngx::today")
}

pub(crate) fn ngx_time_contract() -> HostFunctionSchema {
    host_function_contract("ngx::time")
}

pub(crate) fn ngx_now_contract() -> HostFunctionSchema {
    host_function_contract("ngx::now")
}

pub(crate) fn ngx_update_time_contract() -> HostFunctionSchema {
    host_function_contract("ngx::update_time")
}

pub(crate) fn ngx_localtime_contract() -> HostFunctionSchema {
    host_function_contract("ngx::localtime")
}

pub(crate) fn ngx_utctime_contract() -> HostFunctionSchema {
    host_function_contract("ngx::utctime")
}

pub(crate) fn ngx_cookie_time_contract() -> HostFunctionSchema {
    host_function_contract("ngx::cookie_time")
}

pub(crate) fn ngx_http_time_contract() -> HostFunctionSchema {
    host_function_contract("ngx::http_time")
}

pub(crate) fn ngx_parse_http_time_contract() -> HostFunctionSchema {
    host_function_contract("ngx::parse_http_time")
}

pub(crate) fn ngx_var_get_contract() -> HostFunctionSchema {
    host_function_contract("ngx::var::get")
}

pub(crate) fn ngx_var_set_contract() -> HostFunctionSchema {
    host_function_contract("ngx::var::set")
}

pub(crate) fn ngx_timer_at_contract() -> HostFunctionSchema {
    host_function_contract("ngx::timer::at")
}

pub(crate) fn ngx_timer_every_contract() -> HostFunctionSchema {
    host_function_contract("ngx::timer::every")
}

pub(crate) fn ngx_timer_pending_count_contract() -> HostFunctionSchema {
    host_function_contract("ngx::timer::pending_count")
}

pub(crate) fn ngx_timer_running_count_contract() -> HostFunctionSchema {
    host_function_contract("ngx::timer::running_count")
}

/// Compiler catalog for production ngx imports.
///
/// Schemas, resource types, and named structs come from the explicit
/// [`crate::host::ngx_host_modules`] descriptors, ordered to match
/// [`MANIFEST`] so catalog fingerprints stay stable. Bindings stay
/// name-compatible via [`crate::host::bind_ngx_hosts`] for tests that do not
/// use owned timer dispatch.
pub fn ngx_host_catalog() -> HostApiCatalog {
    let descriptors: Vec<HostFunctionDescriptor> = crate::host::ngx_host_modules()
        .iter()
        .flat_map(|module| module.descriptors())
        .collect();
    let mut by_name = HashMap::with_capacity(descriptors.len());
    for descriptor in descriptors {
        let name = descriptor.schema.name.clone();
        if by_name.insert(name.clone(), descriptor).is_some() {
            panic!("duplicate ngx host descriptor: {name}");
        }
    }
    let mut ordered = Vec::with_capacity(MANIFEST.len());
    for entry in MANIFEST {
        let Some(descriptor) = by_name.remove(entry.name) else {
            panic!("ngx host catalog missing descriptor for {}", entry.name);
        };
        ordered.push(descriptor);
    }
    if !by_name.is_empty() {
        let mut extras: Vec<_> = by_name.keys().cloned().collect();
        extras.sort();
        panic!("ngx host catalog has descriptors not in the manifest: {extras:?}");
    }
    HostFunctionDescriptor::collect_catalog(&ordered)
        .unwrap_or_else(|error| panic!("ngx host catalog must be a valid HostApiCatalog: {error}"))
}

/// Compiler catalog for production ngx scripts.
///
/// It carries exactly the ngx host surface; the pd-vm standard families are
/// deliberately **not** merged, for one reason each:
///
/// - I/O and SQLite are compiler builtins dispatched through the standard
///   composition ([`crate::host::install_standard_runtime`]). Cataloguing their
///   schemas as external host imports would bypass that builtin path and
///   produce unbindable imports.
/// - The pinned pd-vm HTTP client catalog declares its request/response
///   positions as named records (`HttpRequest` / `HttpResponse` / SSE records)
///   and registers its HTTP adapters byte-for-byte from that same catalog.
///   RustScript object literals are exact record types, so a compile-time
///   schema for those positions could never match the documented loose scripts
///   *and* be registered in the owned-dispatch host registry at the same time:
///   a relaxed compile schema has no registered adapter, and the exact schema
///   rejects the documented scripts. The merged catalog therefore leaves the
///   HTTP surface dynamic (`http::client::*` compile as ordinary host imports
///   against the documented `map` interface of `docs/api/standard-async.md`),
///   configuration still validates them by name ([`is_standard_host_import`]),
///   and the pd-vm adapter validates the concrete request contents at runtime
///   and returns the pd-vm contract payload shapes unchanged (duplicate header
///   entries, `HttpHeaderValue` text/bytes variants).
///
/// No standard-family position is relaxed here: with the functions left out
/// there is nothing to relax, so no name, parameter, or result of a standard
/// family can silently become dynamic.
pub fn compile_host_catalog() -> HostApiCatalog {
    ngx_host_catalog()
}

fn standard_host_names() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        vm::http_host_catalog()
            .functions()
            .iter()
            .map(|function| function.name.clone())
            .collect()
    })
}

/// True when `name` belongs to pd-vm's enabled standard host surface.
pub fn is_standard_host_import(name: &str) -> bool {
    standard_host_names().contains(name)
}

/// True when `name` is a catalogued host allowed in `phase`.
pub fn host_allowed_in_phase(name: &str, phase: NgxPhase) -> bool {
    if let Some(entry) = MANIFEST.iter().find(|entry| entry.name == name) {
        return entry.phases.contains(&phase);
    }
    is_standard_host_import(name) && matches!(phase, NgxPhase::Access | NgxPhase::Content)
}

/// Look up a production manifest entry by import name.
pub fn manifest_entry(name: &str) -> Option<&'static HostManifestEntry> {
    MANIFEST.iter().find(|entry| entry.name == name)
}

/// Phase a timer callback executes under.
///
/// The callback VM runs detached from the creating request: there is no live
/// request, so every request-, response-, ctx-, and control-coupled host fails
/// closed. Allowed callback work is log, `ngx::sleep`/`ngx::sleep_ms`, outbound
/// async I/O (cosocket), the shared dictionary, the semaphore family, and the
/// timer family itself.
pub const TIMER_CALLBACK_PHASE: NgxPhase = NgxPhase::Timer;

/// Capabilities a host must not require to run inside a timer callback.
///
/// This is the deny list behind [`host_allowed_in_timer_callback`]: request
/// reads/writes (`ngx::req::*`, `ngx::var::*`, `ngx::header::*`, response
/// metadata), request `ngx.ctx`, response output, and control
/// (`ngx::exit`, `ngx::redirect`).
pub const TIMER_CALLBACK_DENIED_CAPABILITIES: &[HostCapability] = &[
    HostCapability::RequestRead,
    HostCapability::RequestWrite,
    HostCapability::RequestCtx,
    HostCapability::ResponseRead,
    HostCapability::ResponseWrite,
    HostCapability::Control,
];

/// True when a manifest entry may run inside a `ngx::timer` callback.
///
/// Unknown names are rejected: a host must be catalogued in [`MANIFEST`] to
/// reach the callback VM registry.
pub fn host_allowed_in_timer_callback(name: &str) -> bool {
    let Some(entry) = manifest_entry(name) else {
        return false;
    };
    entry
        .capabilities
        .iter()
        .all(|capability| !TIMER_CALLBACK_DENIED_CAPABILITIES.contains(capability))
}

/// Closed positive `log_by_rss_file` allowlist.
///
/// A host is accepted only when every gate matches an explicitly approved
/// family, capability, and name. Future families or capabilities fail the
/// exhaustive `match` at compile time. Unknown names, async hosts, and hosts
/// whose catalog phases omit Log are rejected even if they are Sync.
pub fn host_validates_for_log(entry: &HostManifestEntry) -> bool {
    log_allowlist_accepts(
        entry.name,
        entry.family,
        entry.sync,
        entry.phases,
        entry.capabilities,
    )
}

/// Same closed allowlist as [`host_validates_for_log`], exposed so tests can
/// construct synthetic future Sync+Log hosts without going through MANIFEST.
pub fn log_allowlist_accepts(
    name: &str,
    family: HostFamily,
    sync: HostSyncKind,
    phases: &[NgxPhase],
    capabilities: &[HostCapability],
) -> bool {
    if sync != HostSyncKind::Sync {
        return false;
    }
    if !phases.contains(&NgxPhase::Log) {
        return false;
    }
    if !log_family_allowed(family) {
        return false;
    }
    if !capabilities.iter().copied().all(log_capability_allowed) {
        return false;
    }
    log_name_allowed(name)
}

fn log_family_allowed(family: HostFamily) -> bool {
    match family {
        HostFamily::Codec
        | HostFamily::Time
        | HostFamily::Meta
        | HostFamily::Log
        | HostFamily::Constant
        | HostFamily::Request
        | HostFamily::RequestCtx
        | HostFamily::SharedDict
        | HostFamily::Response => true,
        HostFamily::Timer | HostFamily::Semaphore | HostFamily::Cosocket | HostFamily::Control => {
            false
        }
    }
}

fn log_capability_allowed(cap: HostCapability) -> bool {
    match cap {
        HostCapability::Codec
        | HostCapability::Time
        | HostCapability::Meta
        | HostCapability::Log
        | HostCapability::Constant
        | HostCapability::RequestRead
        | HostCapability::RequestCtx
        | HostCapability::SharedDict
        | HostCapability::ResponseRead => true,
        HostCapability::Timer
        | HostCapability::Semaphore
        | HostCapability::Cosocket
        | HostCapability::Control
        | HostCapability::RequestWrite
        | HostCapability::ResponseWrite => false,
    }
}

/// Explicitly approved import names. A new host in an already-allowed family
/// still needs a review entry here before `log_by_rss_file` will compile it.
pub const LOG_ALLOWED_NAMES: &[&str] = &[
    NGX_ESCAPE_URI_NAME,
    NGX_UNESCAPE_URI_NAME,
    NGX_ENCODE_ARGS_NAME,
    NGX_DECODE_ARGS_NAME,
    NGX_ENCODE_BASE64_NAME,
    NGX_DECODE_BASE64_NAME,
    NGX_DECODE_BASE64MIME_NAME,
    NGX_CRC32_SHORT_NAME,
    NGX_CRC32_LONG_NAME,
    NGX_HMAC_SHA1_NAME,
    NGX_MD5_NAME,
    NGX_MD5_BIN_NAME,
    NGX_SHA1_BIN_NAME,
    NGX_QUOTE_SQL_STR_NAME,
    NGX_TODAY_NAME,
    NGX_TIME_NAME,
    NGX_NOW_NAME,
    NGX_UPDATE_TIME_NAME,
    NGX_LOCALTIME_NAME,
    NGX_UTCTIME_NAME,
    NGX_COOKIE_TIME_NAME,
    NGX_HTTP_TIME_NAME,
    NGX_PARSE_HTTP_TIME_NAME,
    "ngx::get_phase",
    "ngx::config::subsystem",
    "ngx::config::debug",
    "ngx::config::prefix",
    "ngx::config::nginx_version",
    "ngx::config::nginx_configure",
    "ngx::config::ngx_rustscript_version",
    "ngx::worker::exiting",
    "ngx::worker::pid",
    "ngx::worker::pids",
    "ngx::worker::count",
    "ngx::worker::id",
    "ngx::log",
    "ngx::core::ok",
    "ngx::core::error",
    "ngx::core::again",
    "ngx::core::done",
    "ngx::core::declined",
    "ngx::core::null_value",
    "ngx::http::get",
    "ngx::http::head",
    "ngx::http::put",
    "ngx::http::post",
    "ngx::http::delete",
    "ngx::http::options",
    "ngx::http::mkcol",
    "ngx::http::copy",
    "ngx::http::move",
    "ngx::http::propfind",
    "ngx::http::proppatch",
    "ngx::http::lock",
    "ngx::http::unlock",
    "ngx::http::patch",
    "ngx::http::trace",
    "ngx::http::status_continue",
    "ngx::http::status_switching_protocols",
    "ngx::http::status_ok",
    "ngx::http::status_created",
    "ngx::http::status_accepted",
    "ngx::http::status_no_content",
    "ngx::http::status_partial_content",
    "ngx::http::status_special_response",
    "ngx::http::status_moved_permanently",
    "ngx::http::status_moved_temporarily",
    "ngx::http::status_see_other",
    "ngx::http::status_not_modified",
    "ngx::http::status_temporary_redirect",
    "ngx::http::status_permanent_redirect",
    "ngx::http::status_bad_request",
    "ngx::http::status_unauthorized",
    "ngx::http::status_payment_required",
    "ngx::http::status_forbidden",
    "ngx::http::status_not_found",
    "ngx::http::status_not_allowed",
    "ngx::http::status_not_acceptable",
    "ngx::http::status_request_timeout",
    "ngx::http::status_conflict",
    "ngx::http::status_gone",
    "ngx::http::status_upgrade_required",
    "ngx::http::status_too_many_requests",
    "ngx::http::status_illegal",
    "ngx::http::status_close",
    "ngx::http::status_internal_server_error",
    "ngx::http::status_not_implemented",
    "ngx::http::status_bad_gateway",
    "ngx::http::status_service_unavailable",
    "ngx::http::status_gateway_timeout",
    "ngx::http::status_version_not_supported",
    "ngx::http::status_insufficient_storage",
    "ngx::log_level::stderr",
    "ngx::log_level::emerg",
    "ngx::log_level::alert",
    "ngx::log_level::crit",
    "ngx::log_level::error",
    "ngx::log_level::warn",
    "ngx::log_level::notice",
    "ngx::log_level::info",
    "ngx::log_level::debug",
    NGX_REQ_GET_METHOD_NAME,
    NGX_REQ_GET_HEADERS_NAME,
    NGX_REQ_GET_URI_ARGS_NAME,
    NGX_REQ_GET_POST_ARGS_NAME,
    NGX_REQ_GET_BODY_DATA_NAME,
    NGX_REQ_IS_INTERNAL_NAME,
    NGX_REQ_START_TIME_NAME,
    NGX_IS_SUBREQUEST_NAME,
    NGX_VAR_GET_NAME,
    NGX_STATUS_GET_NAME,
    NGX_HEADER_GET_NAME,
    NGX_CTX_GET_NAME,
    NGX_CTX_SET_NAME,
    NGX_CTX_DELETE_NAME,
    NGX_CTX_CLEAR_NAME,
    NGX_SHARED_DICT_NAME,
    NGX_SHARED_GET_NAME,
    NGX_SHARED_GET_STALE_NAME,
    NGX_SHARED_SET_NAME,
    NGX_SHARED_SAFE_SET_NAME,
    NGX_SHARED_ADD_NAME,
    NGX_SHARED_SAFE_ADD_NAME,
    NGX_SHARED_REPLACE_NAME,
    NGX_SHARED_DELETE_NAME,
    NGX_SHARED_INCR_NAME,
    NGX_SHARED_TTL_NAME,
    NGX_SHARED_EXPIRE_NAME,
    NGX_SHARED_CAPACITY_NAME,
    NGX_SHARED_FREE_SPACE_NAME,
    NGX_SHARED_LPUSH_NAME,
    NGX_SHARED_RPUSH_NAME,
    NGX_SHARED_LPOP_NAME,
    NGX_SHARED_RPOP_NAME,
    NGX_SHARED_LLEN_NAME,
    NGX_SHARED_FLUSH_ALL_NAME,
    NGX_SHARED_FLUSH_EXPIRED_NAME,
    NGX_SHARED_GET_KEYS_NAME,
];

fn log_name_allowed(name: &str) -> bool {
    LOG_ALLOWED_NAMES.contains(&name)
}
