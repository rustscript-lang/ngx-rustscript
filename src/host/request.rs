// Request-read hosts. Copy ngx_str/list bytes into owned VM values before
// returning; never expose request, pool, or list pointers.

use nginx_sys::{
    ngx_http_request_t, ngx_list_part_t, ngx_list_t, ngx_str_t, ngx_table_elt_t, ngx_uint_t,
    NGX_HTTP_VERSION_10, NGX_HTTP_VERSION_11, NGX_HTTP_VERSION_20, NGX_HTTP_VERSION_30,
    NGX_HTTP_VERSION_9,
};
use ngx::http::Request;
use pd_host_function::pd_host_function;
use vm::{Value, VmError, VmMap};

use super::super::VmResult;
use super::{CallOutcome, IntoHostCallOutcome};
use crate::host::{current_identity, with_current_request, ScopeError};

const MAX_COPY_BYTES: usize = 65_536;
const MAX_HEADER_FIELD_BYTES: usize = 8_192;
const MAX_LIST_PARTS: usize = 256;
const DEFAULT_MAX_HEADERS: i64 = 100;
const HARD_MAX_HEADERS: i64 = 1_024;
const DEFAULT_MAX_ARGS: i64 = 100;
const HARD_MAX_ARGS: i64 = 1_024;

fn ret<T: IntoHostCallOutcome>(value: T) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn scope_err(err: ScopeError) -> VmError {
    VmError::HostError(err.to_string())
}

fn with_request<T>(f: impl FnOnce(&Request) -> VmResult<T>) -> VmResult<T> {
    let identity = current_identity().map_err(scope_err)?;
    with_current_request(identity, f).map_err(scope_err)?
}

fn bytes_as_string(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => err.into_bytes().into_iter().map(char::from).collect(),
    }
}

fn copy_ngx_str(s: ngx_str_t, cap: usize, name: &str) -> VmResult<Vec<u8>> {
    if s.data.is_null() || s.len == 0 {
        return Ok(Vec::new());
    }
    if s.len > cap {
        return Err(VmError::HostError(format!(
            "{name} exceeds the {cap}-byte copy limit"
        )));
    }
    // SAFETY: caller validated data/len against a live request or test fixture.
    Ok(unsafe { std::slice::from_raw_parts(s.data, s.len) }.to_vec())
}

fn copy_ptr_len(ptr: *const u8, len: usize, cap: usize, name: &str) -> VmResult<Vec<u8>> {
    if ptr.is_null() || len == 0 {
        return Ok(Vec::new());
    }
    if len > cap {
        return Err(VmError::HostError(format!(
            "{name} exceeds the {cap}-byte copy limit"
        )));
    }
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
}

fn optional_int(value: &Value, name: &str) -> VmResult<Option<i64>> {
    match value {
        Value::Null => Ok(None),
        Value::Int(number) => Ok(Some(*number)),
        _ => Err(VmError::HostError(format!("{name} expects int or null"))),
    }
}

fn optional_bool(value: &Value, name: &str) -> VmResult<Option<bool>> {
    match value {
        Value::Null => Ok(None),
        Value::Bool(flag) => Ok(Some(*flag)),
        _ => Err(VmError::HostError(format!("{name} expects bool or null"))),
    }
}

fn clamp_count(value: &Value, name: &str, default: i64, hard: i64) -> VmResult<usize> {
    match optional_int(value, name)? {
        None => Ok(default as usize),
        Some(0) => Ok(hard as usize),
        Some(n) if n < 0 => Err(VmError::HostError(format!("{name} must be non-negative"))),
        Some(n) if n > hard => Err(VmError::HostError(format!("{name} exceeds {hard}"))),
        Some(n) => Ok(n as usize),
    }
}

fn unescape_bytes(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    let mut index = 0usize;
    while index < src.len() {
        match src[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < src.len() => {
                let hi = src[index + 1];
                let lo = src[index + 2];
                match (from_hex(hi), from_hex(lo)) {
                    (Some(h), Some(l)) => {
                        out.push((h << 4) | l);
                        index += 3;
                    }
                    _ => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    out
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn insert_decoded_arg(args: &mut VmMap, key: String, value: Value) {
    let key_value = Value::string(key);
    match args.get(&key_value) {
        None => {
            args.insert(key_value, value);
        }
        Some(Value::Array(existing)) => {
            let mut items = existing.as_ref().clone();
            items.push(value);
            args.insert(key_value, Value::array(items));
        }
        Some(previous) => {
            let previous = previous.clone();
            args.insert(key_value, Value::array(vec![previous, value]));
        }
    }
}

fn decode_query(query: &str, max_args: usize) -> (VmMap, bool) {
    let mut args = VmMap::new();
    let mut seen = 0usize;
    let mut truncated = false;
    if !query.is_empty() {
        for part in query.split('&') {
            if seen >= max_args {
                truncated = true;
                break;
            }
            if part.is_empty() {
                continue;
            }
            let (raw_key, value) = match part.split_once('=') {
                Some((key, raw_value)) => (
                    key,
                    Value::string(bytes_as_string(unescape_bytes(raw_value.as_bytes()))),
                ),
                None => (part, Value::Bool(true)),
            };
            let key = bytes_as_string(unescape_bytes(raw_key.as_bytes()));
            if key.is_empty() {
                continue;
            }
            insert_decoded_arg(&mut args, key, value);
            seen += 1;
        }
    }
    (args, truncated)
}

fn result_map(inner_key: &str, inner: VmMap, truncated: bool) -> VmMap {
    let mut result = VmMap::new();
    result.insert(Value::string(inner_key), Value::Map(std::sync::Arc::new(inner)));
    result.insert(Value::string("truncated"), Value::Bool(truncated));
    result
}

fn http_version_number(version: ngx_uint_t) -> Option<f64> {
    if version == NGX_HTTP_VERSION_9 as ngx_uint_t {
        Some(0.9)
    } else if version == NGX_HTTP_VERSION_10 as ngx_uint_t {
        Some(1.0)
    } else if version == NGX_HTTP_VERSION_11 as ngx_uint_t {
        Some(1.1)
    } else if version == NGX_HTTP_VERSION_20 as ngx_uint_t {
        Some(2.0)
    } else if version == NGX_HTTP_VERSION_30 as ngx_uint_t {
        Some(3.0)
    } else {
        None
    }
}

fn lowercase_owned(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().copied().map(|byte| byte.to_ascii_lowercase()).collect()
}

fn collect_headers(list: &ngx_list_t, max: usize, raw: bool) -> VmResult<(VmMap, bool)> {
    let mut headers = VmMap::new();
    let mut seen = 0usize;
    let mut truncated = false;
    let mut total_bytes = 0usize;
    let mut part = &list.part as *const ngx_list_part_t;
    let mut seen_parts: [*const ngx_list_part_t; MAX_LIST_PARTS] =
        [std::ptr::null(); MAX_LIST_PARTS];
    let mut part_count = 0usize;

    while !part.is_null() {
        if part_count >= MAX_LIST_PARTS {
            truncated = true;
            break;
        }
        if seen_parts[..part_count]
            .iter()
            .any(|&seen_part| std::ptr::eq(seen_part, part))
        {
            break;
        }
        seen_parts[part_count] = part;
        part_count += 1;

        // SAFETY: part is a non-null list node; cycle/count caps bound traversal.
        let part_ref = unsafe { &*part };
        if part_ref.nelts > 0 {
            if part_ref.elts.is_null() || part_ref.nelts > HARD_MAX_HEADERS as usize {
                break;
            }
            let elts = unsafe {
                std::slice::from_raw_parts(
                    part_ref.elts.cast::<ngx_table_elt_t>(),
                    part_ref.nelts,
                )
            };
            for header in elts {
                if header.hash == 0 || header.key.len == 0 {
                    continue;
                }
                if seen >= max {
                    truncated = true;
                    break;
                }
                let key_bytes = if raw {
                    copy_ngx_str(header.key, MAX_HEADER_FIELD_BYTES, "ngx::req::get_headers")?
                } else if header.lowcase_key.is_null() {
                    lowercase_owned(&copy_ngx_str(
                        header.key,
                        MAX_HEADER_FIELD_BYTES,
                        "ngx::req::get_headers",
                    )?)
                } else {
                    copy_ptr_len(
                        header.lowcase_key,
                        header.key.len,
                        MAX_HEADER_FIELD_BYTES,
                        "ngx::req::get_headers",
                    )?
                };
                let value_bytes =
                    copy_ngx_str(header.value, MAX_HEADER_FIELD_BYTES, "ngx::req::get_headers")?;
                total_bytes = total_bytes
                    .saturating_add(key_bytes.len())
                    .saturating_add(value_bytes.len());
                if total_bytes > MAX_COPY_BYTES {
                    truncated = true;
                    break;
                }
                insert_decoded_arg(
                    &mut headers,
                    bytes_as_string(key_bytes),
                    Value::string(bytes_as_string(value_bytes)),
                );
                seen += 1;
            }
        }
        if truncated {
            break;
        }
        part = part_ref.next;
    }

    Ok((headers, truncated))
}

fn copy_raw_header(raw: &ngx_http_request_t, no_request_line: bool) -> VmResult<String> {
    if !raw.stream.is_null() {
        return Err(VmError::HostError(
            "ngx::req::raw_header does not support HTTP/2 requests".to_string(),
        ));
    }
    if raw.request_line.len == 0 || raw.request_line.data.is_null() {
        return Ok(String::new());
    }

    let mut copied = if !raw.request_start.is_null() && !raw.header_end.is_null() {
        let start = raw.request_start as usize;
        let end = raw.header_end as usize;
        if end >= start {
            copy_ptr_len(
                raw.request_start,
                end - start,
                MAX_COPY_BYTES,
                "ngx::req::raw_header",
            )?
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    if copied.is_empty() {
        copied = copy_ngx_str(
            raw.request_line,
            MAX_COPY_BYTES,
            "ngx::req::raw_header",
        )?;
        copied.extend_from_slice(b"\r\n");
    }

    if no_request_line {
        let mut skip = raw.request_line.len.min(copied.len());
        if skip < copied.len() && copied[skip] == b'\r' {
            skip += 1;
            if skip < copied.len() && copied[skip] == b'\n' {
                skip += 1;
            }
        } else if skip < copied.len() && copied[skip] == b'\n' {
            skip += 1;
        }
        copied = copied.split_off(skip);
    }

    for byte in &mut copied {
        if *byte == 0 {
            *byte = b' ';
        }
    }

    Ok(bytes_as_string(copied))
}

/// Current request method name. Custom methods keep the textual token.
#[pd_host_function(name = "ngx::req::get_method")]
pub fn get_method() -> VmResult<CallOutcome> {
    with_request(|request| {
        let raw = request.as_ref();
        let bytes = copy_ngx_str(raw.method_name, 256, "ngx::req::get_method")?;
        ret(bytes_as_string(bytes))
    })
}

/// HTTP version as 0.9/1.0/1.1/2.0/3.0, or `null` if unrecognized.
#[pd_host_function(name = "ngx::req::http_version")]
pub fn http_version() -> VmResult<CallOutcome> {
    with_request(|request| ret(http_version_number(request.as_ref().http_version)))
}

/// Original request header bytes. `no_request_line` is `null`/`false`/`true`.
#[pd_host_function(name = "ngx::req::raw_header")]
pub fn raw_header(no_request_line: &Value) -> VmResult<CallOutcome> {
    let skip = optional_bool(no_request_line, "ngx::req::raw_header no_request_line")?.unwrap_or(false);
    with_request(|request| ret(copy_raw_header(request.as_ref(), skip)?))
}

/// Request headers as `{ headers, truncated }`. `raw` keeps original names.
#[pd_host_function(name = "ngx::req::get_headers")]
pub fn get_headers(max_headers: &Value, raw: &Value) -> VmResult<CallOutcome> {
    let max = clamp_count(
        max_headers,
        "ngx::req::get_headers max_headers",
        DEFAULT_MAX_HEADERS,
        HARD_MAX_HEADERS,
    )?;
    let raw_names = optional_bool(raw, "ngx::req::get_headers raw")?.unwrap_or(false);
    with_request(|request| {
        let (headers, truncated) = collect_headers(&request.as_ref().headers_in.headers, max, raw_names)?;
        ret(result_map("headers", headers, truncated))
    })
}

/// URI query args as `{ args, truncated }`, matching `ngx::decode_args`.
#[pd_host_function(name = "ngx::req::get_uri_args")]
pub fn get_uri_args(max_args: &Value) -> VmResult<CallOutcome> {
    let max = clamp_count(
        max_args,
        "ngx::req::get_uri_args max_args",
        DEFAULT_MAX_ARGS,
        HARD_MAX_ARGS,
    )?;
    with_request(|request| {
        let bytes = copy_ngx_str(request.as_ref().args, MAX_COPY_BYTES, "ngx::req::get_uri_args")?;
        let query = bytes_as_string(bytes);
        let (args, truncated) = decode_query(&query, max);
        ret(result_map("args", args, truncated))
    })
}

/// POST args. Unread bodies error and defer full behavior to Task 10.
#[pd_host_function(name = "ngx::req::get_post_args")]
pub fn get_post_args(_max_args: &Value) -> VmResult<CallOutcome> {
    with_request(|request| {
        if request.as_ref().request_body.is_null() {
            return Err(VmError::HostError(
                "ngx::req::get_post_args requires the request body to be read first (Task 10)"
                    .to_string(),
            ));
        }
        Err(VmError::HostError(
            "ngx::req::get_post_args full body parsing is deferred to Task 10".to_string(),
        ))
    })
}

/// True when NGINX marked the request internal.
#[pd_host_function(name = "ngx::req::is_internal")]
pub fn is_internal() -> VmResult<CallOutcome> {
    with_request(|request| ret(request.as_ref().internal() != 0))
}

/// Request start time as seconds + milliseconds / 1000.
#[pd_host_function(name = "ngx::req::start_time")]
pub fn start_time() -> VmResult<CallOutcome> {
    with_request(|request| {
        let raw = request.as_ref();
        ret(raw.start_sec as f64 + (raw.start_msec as f64) / 1000.0)
    })
}

/// True when the current request is not the main request.
#[pd_host_function(name = "ngx::is_subrequest")]
pub fn is_subrequest() -> VmResult<CallOutcome> {
    with_request(|request| {
        let raw = request.as_ref();
        if raw.main.is_null() {
            return ret(false);
        }
        ret(!std::ptr::eq(
            raw as *const ngx_http_request_t,
            raw.main as *const ngx_http_request_t,
        ))
    })
}

/// True when NGINX has already sent response headers.
#[pd_host_function(name = "ngx::headers_sent")]
pub fn headers_sent() -> VmResult<CallOutcome> {
    with_request(|request| ret(request.as_ref().header_sent() != 0))
}
