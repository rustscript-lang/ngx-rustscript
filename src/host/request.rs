// Request-read hosts. Copy ngx_str/list bytes into owned VM values before
// returning; never expose request, pool, or list pointers.

use core::ffi::c_uint;

use nginx_sys::{
    ngx_buf_t, ngx_chain_t, ngx_http_connection_t, ngx_http_request_t, ngx_int_t, ngx_list_part_t,
    ngx_list_push, ngx_list_t, ngx_palloc, ngx_pool_t, ngx_str_t, ngx_table_elt_t, ngx_uint_t, off_t,
    NGX_HTTP_COPY, NGX_HTTP_DELETE, NGX_HTTP_GET, NGX_HTTP_HEAD, NGX_HTTP_LOCK, NGX_HTTP_MKCOL,
    NGX_HTTP_MOVE, NGX_HTTP_OPTIONS, NGX_HTTP_PATCH, NGX_HTTP_POST, NGX_HTTP_PROPFIND,
    NGX_HTTP_PROPPATCH, NGX_HTTP_PUT, NGX_HTTP_TRACE, NGX_HTTP_UNLOCK, NGX_HTTP_VERSION_10,
    NGX_HTTP_VERSION_11, NGX_HTTP_VERSION_20, NGX_HTTP_VERSION_30, NGX_HTTP_VERSION_9,
};
use ngx::http::Request;
use pd_host_function::pd_host_function;
use vm::{Value, VmError, VmMap};

use super::super::VmResult;
use super::{CallOutcome, IntoHostCallOutcome};
use crate::host::{
    current_identity, with_current_request, with_current_request_mut, ScopeError,
};

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

fn with_request_write<T>(f: impl FnOnce(&mut Request) -> VmResult<T>) -> VmResult<T> {
    let identity = current_identity().map_err(scope_err)?;
    with_current_request_mut(identity, f).map_err(scope_err)?
}

unsafe extern "C" {
    fn ngx_rustscript_c_request_is_internal(r: *const ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_request_headers_sent(r: *const ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_request_reading_body(r: *const ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_request_discard_body(r: *const ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_request_set_uri_flags(
        r: *mut ngx_http_request_t,
        internal: c_uint,
        valid_unparsed_uri: c_uint,
        valid_location: c_uint,
        uri_changed: c_uint,
    );
    fn ngx_rustscript_c_request_set_valid_unparsed_uri(r: *mut ngx_http_request_t, value: c_uint);
    fn ngx_rustscript_c_request_set_exten(r: *mut ngx_http_request_t);
    fn ngx_rustscript_c_validate_host(
        pool: *mut ngx_pool_t,
        data: *mut u8,
        len: usize,
        server_out: *mut ngx_str_t,
    ) -> ngx_int_t;
    fn ngx_rustscript_c_invalidate_cached_host(r: *mut ngx_http_request_t);
    fn ngx_rustscript_c_headers_in_apply_user_agent(
        r: *mut ngx_http_request_t,
        h: *mut ngx_table_elt_t,
    );
    fn ngx_rustscript_c_headers_in_apply_connection(
        r: *mut ngx_http_request_t,
        h: *mut ngx_table_elt_t,
    );
    fn ngx_rustscript_c_parse_content_length(
        data: *const u8,
        len: usize,
        out: *mut off_t,
    ) -> ngx_int_t;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ngx_rustscript_request_is_internal(
    r: *const ngx_http_request_t,
) -> c_uint {
    unsafe { ngx_rustscript_c_request_is_internal(r) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ngx_rustscript_request_headers_sent(
    r: *const ngx_http_request_t,
) -> c_uint {
    unsafe { ngx_rustscript_c_request_headers_sent(r) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ngx_rustscript_request_reading_body(
    r: *const ngx_http_request_t,
) -> c_uint {
    unsafe { ngx_rustscript_c_request_reading_body(r) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ngx_rustscript_request_discard_body(
    r: *const ngx_http_request_t,
) -> c_uint {
    unsafe { ngx_rustscript_c_request_discard_body(r) }
}

fn request_flag(
    raw: &ngx_http_request_t,
    read: unsafe extern "C" fn(*const ngx_http_request_t) -> c_uint,
) -> bool {
    unsafe { read(raw) != 0 }
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
    bytes
        .iter()
        .copied()
        .map(|byte| byte.to_ascii_lowercase())
        .collect()
}

fn header_error(message: &str) -> VmError {
    VmError::HostError(format!("ngx::req::get_headers {message}"))
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
            return Err(header_error("exceeds the list-part safety cap"));
        }
        if seen_parts[..part_count]
            .iter()
            .any(|&seen_part| std::ptr::eq(seen_part, part))
        {
            return Err(header_error("malformed cyclic header list"));
        }
        seen_parts[part_count] = part;
        part_count += 1;

        // SAFETY: part is a non-null list node; cycle/count caps bound traversal.
        let part_ref = unsafe { &*part };
        if part_ref.nelts > 0 {
            if part_ref.elts.is_null() {
                return Err(header_error("malformed null header elts"));
            }
            if part_ref.nelts > HARD_MAX_HEADERS as usize {
                return Err(header_error("invalid header part nelts"));
            }
            if list.size != 0 && list.size != std::mem::size_of::<ngx_table_elt_t>() {
                return Err(header_error("impossible header list metadata"));
            }
            let elts = unsafe {
                std::slice::from_raw_parts(part_ref.elts.cast::<ngx_table_elt_t>(), part_ref.nelts)
            };
            for header in elts {
                if header.hash == 0 || header.key.len == 0 {
                    continue;
                }
                if header.key.data.is_null() {
                    return Err(header_error("malformed header key"));
                }
                if header.value.len > 0 && header.value.data.is_null() {
                    return Err(header_error("malformed header value"));
                }
                if seen >= max {
                    truncated = true;
                    break;
                }
                let add = header.key.len.saturating_add(header.value.len);
                if total_bytes.saturating_add(add) > MAX_COPY_BYTES {
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

fn main_request(raw: &ngx_http_request_t) -> &ngx_http_request_t {
    if raw.main.is_null() {
        raw
    } else {
        // SAFETY: NGINX sets r->main to the live main request or self.
        unsafe { &*raw.main }
    }
}

fn raw_header_error(message: &str) -> VmError {
    VmError::HostError(format!("ngx::req::raw_header {message}"))
}

fn validate_header_buf(buf: &ngx_buf_t) -> VmResult<usize> {
    if buf.start.is_null() || buf.pos.is_null() {
        return Err(raw_header_error("malformed header buffer"));
    }
    let start = buf.start as usize;
    let pos = buf.pos as usize;
    if pos < start {
        return Err(raw_header_error("inverted header buffer range"));
    }
    let len = pos - start;
    if len > MAX_COPY_BYTES {
        return Err(raw_header_error("exceeds the 65536-byte copy limit"));
    }
    Ok(len)
}

fn line_break_len(buf: &ngx_buf_t, line: ngx_str_t) -> Option<usize> {
    if line.data.is_null() || buf.start.is_null() || buf.pos.is_null() {
        return None;
    }
    let start = buf.start as usize;
    let pos = buf.pos as usize;
    let data = line.data as usize;
    let after = data.saturating_add(line.len);
    if data < start || after > pos {
        return None;
    }
    if after >= pos {
        return None;
    }
    // SAFETY: after < pos and the byte lives in this validated buffer.
    let ch = unsafe { *line.data.add(line.len) };
    if ch == b'\r' {
        if after.saturating_add(1) > pos {
            return None;
        }
        Some(2)
    } else {
        Some(1)
    }
}

fn contains_request_line(buf: &ngx_buf_t, line: ngx_str_t, line_break_len: usize) -> bool {
    if line.data.is_null() || buf.start.is_null() || buf.pos.is_null() {
        return false;
    }
    let start = buf.start as usize;
    let pos = buf.pos as usize;
    let data = line.data as usize;
    let end = data.saturating_add(line.len).saturating_add(line_break_len);
    data >= start && end <= pos && end >= data
}

fn collect_busy_bufs(hc: &ngx_http_connection_t) -> VmResult<Vec<*mut ngx_buf_t>> {
    if hc.nbusy == 0 {
        return Ok(Vec::new());
    }
    if hc.nbusy < 0 {
        return Err(raw_header_error("impossible busy-buffer metadata"));
    }
    let expected = hc.nbusy as usize;
    if expected > MAX_LIST_PARTS {
        return Err(raw_header_error("busy-buffer chain exceeds the safety cap"));
    }
    if hc.busy.is_null() {
        return Err(raw_header_error("malformed null busy-buffer chain"));
    }
    let mut out = Vec::new();
    let mut seen: Vec<*mut ngx_chain_t> = Vec::new();
    let mut cl = hc.busy;
    while !cl.is_null() {
        if seen.iter().any(|&ptr| std::ptr::eq(ptr, cl)) {
            return Err(raw_header_error("malformed cyclic header buffer chain"));
        }
        if seen.len() >= MAX_LIST_PARTS {
            return Err(raw_header_error("busy-buffer chain exceeds the safety cap"));
        }
        seen.push(cl);
        // SAFETY: cl is a non-null chain node not previously visited.
        let chain = unsafe { &*cl };
        if chain.buf.is_null() {
            return Err(raw_header_error("malformed null header buffer"));
        }
        out.push(chain.buf);
        cl = chain.next;
    }
    if out.len() != expected {
        return Err(raw_header_error("impossible busy-buffer metadata"));
    }
    Ok(out)
}

fn restore_parser_nuls(bytes: &mut [u8], busy_style: bool) {
    let mut nuls = 0usize;
    let len = bytes.len();
    for idx in 0..len {
        if bytes[idx] != 0 {
            continue;
        }
        nuls += 1;
        if busy_style && idx + 1 == len {
            bytes[idx] = b'\n';
        } else if idx + 1 < len && bytes[idx + 1] == b'\n' {
            bytes[idx] = b'\r';
        } else if nuls % 2 == 1 {
            bytes[idx] = b':';
        } else {
            bytes[idx] = b'\n';
        }
    }
}

fn skip_truncated_tail(bytes: &mut Vec<u8>, from: usize) {
    while bytes.len() > from {
        match bytes.last() {
            Some(&b'\n') | Some(&0) => break,
            Some(_) => {
                bytes.pop();
            }
            None => break,
        }
    }
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|window| window == needle)
}

fn strip_preread_body(bytes: &mut Vec<u8>) {
    if let Some(idx) = find_subslice(bytes, b"\r\n\r\n") {
        bytes.truncate(idx + 4);
        return;
    }
    if let Some(idx) = find_subslice(bytes, b"\r\n\n") {
        bytes.truncate(idx + 3);
        return;
    }
    if let Some(idx) = find_subslice(bytes, b"\n\r\n") {
        bytes.truncate(idx + 3);
        return;
    }
    if bytes.len() < 2 {
        return;
    }
    let mut i = bytes.len() - 1;
    while i >= 2 {
        if bytes[i] == b'\n' && bytes[i - 1] == b'\r' {
            bytes[i - 1] = b'\n';
            bytes.truncate(i + 1);
            return;
        }
        if bytes[i] == b'\n' && bytes[i - 1] == b'\n' {
            bytes.truncate(i + 1);
            return;
        }
        i -= 1;
    }
}

fn copy_checked(ptr: *const u8, len: usize, name: &str) -> VmResult<&[u8]> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        return Err(raw_header_error("malformed header buffer"));
    }
    if len > MAX_COPY_BYTES {
        return Err(VmError::HostError(format!(
            "{name} exceeds the {MAX_COPY_BYTES}-byte copy limit"
        )));
    }
    // SAFETY: caller validated ptr+len against a live header buffer range.
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
}

fn copy_raw_header(raw: &ngx_http_request_t, no_request_line: bool) -> VmResult<String> {
    let mr = main_request(raw);
    if !mr.stream.is_null() {
        return Err(raw_header_error("does not support HTTP/2 requests"));
    }
    if mr.http_version == NGX_HTTP_VERSION_30 as ngx_uint_t || !mr.v3_parse.is_null() {
        return Err(raw_header_error("does not support HTTP/3 requests"));
    }
    if mr.connection.is_null() {
        if mr.request_line.len == 0 || mr.request_line.data.is_null() {
            return Ok(String::new());
        }
        return Err(raw_header_error("missing request connection header buffers"));
    }
    // SAFETY: connection pointer is non-null on a live/main request fixture.
    let connection = unsafe { &*mr.connection };
    if mr.request_line.len == 0 || mr.request_line.data.is_null() {
        return Ok(String::new());
    }
    if connection.buffer.is_null() {
        return Err(raw_header_error("missing connection header buffer"));
    }
    if mr.http_connection.is_null() {
        return Err(raw_header_error("missing request connection header buffers"));
    }
    let hc = unsafe { &*mr.http_connection };
    let conn_buf = unsafe { &*connection.buffer };
    validate_header_buf(conn_buf)?;
    let busy = collect_busy_bufs(hc)?;
    for bufp in &busy {
        // SAFETY: collect_busy_bufs rejected null buf pointers.
        validate_header_buf(unsafe { &**bufp })?;
    }

    let mut linebreak = line_break_len(conn_buf, mr.request_line);
    if linebreak.is_none() {
        for bufp in busy.iter().rev() {
            linebreak = line_break_len(unsafe { &**bufp }, mr.request_line);
            if linebreak.is_some() {
                break;
            }
        }
    }
    let linebreak = linebreak.ok_or_else(|| {
        raw_header_error("request line is outside header buffers")
    })?;

    let mut first: *const ngx_buf_t = std::ptr::null();
    let mut size = 0usize;
    if contains_request_line(conn_buf, mr.request_line, linebreak) {
        first = connection.buffer;
        size = size.saturating_add(
            (conn_buf.pos as usize).saturating_sub(mr.request_line.data as usize),
        );
    }
    if !busy.is_empty() {
        for bufp in busy.iter().rev() {
            let buf = unsafe { &**bufp };
            let len = validate_header_buf(buf)?;
            if first.is_null() {
                if !contains_request_line(buf, mr.request_line, linebreak) {
                    continue;
                }
                first = *bufp;
            }
            size = size.saturating_add(len);
        }
    }
    if first.is_null() {
        return Err(raw_header_error("request line is outside header buffers"));
    }
    if size > MAX_COPY_BYTES {
        return Err(raw_header_error("exceeds the 65536-byte copy limit"));
    }

    let mut data = Vec::with_capacity(size);
    let mut found = false;
    if std::ptr::eq(first, connection.buffer) {
        found = true;
        let pos = conn_buf.pos as usize;
        let start = if no_request_line {
            (mr.request_line.data as usize)
                .saturating_add(mr.request_line.len)
                .saturating_add(linebreak)
        } else {
            mr.request_line.data as usize
        };
        if start > pos {
            return Err(raw_header_error("inverted header buffer range"));
        }
        data.extend_from_slice(copy_checked(
            start as *const u8,
            pos - start,
            "ngx::req::raw_header",
        )?);
        if mr.header_in.is_null() || !std::ptr::eq(connection.buffer, mr.header_in) {
            skip_truncated_tail(&mut data, 0);
        }
        restore_parser_nuls(&mut data, false);
    }

    if !busy.is_empty() {
        for bufp in busy.iter().rev() {
            let buf = unsafe { &**bufp };
            if !found {
                if !std::ptr::eq(*bufp as *const ngx_buf_t, first) {
                    continue;
                }
                found = true;
            }
            let mark = data.len();
            let pos = buf.pos as usize;
            if std::ptr::eq(*bufp as *const ngx_buf_t, first) {
                let start = if no_request_line {
                    (mr.request_line.data as usize)
                        .saturating_add(mr.request_line.len)
                        .saturating_add(linebreak)
                } else {
                    mr.request_line.data as usize
                };
                if start > pos {
                    return Err(raw_header_error("inverted header buffer range"));
                }
                data.extend_from_slice(copy_checked(
                    start as *const u8,
                    pos - start,
                    "ngx::req::raw_header",
                )?);
            } else {
                data.extend_from_slice(copy_checked(
                    buf.start,
                    pos - buf.start as usize,
                    "ngx::req::raw_header",
                )?);
            }
            skip_truncated_tail(&mut data, mark);
            restore_parser_nuls(&mut data[mark..], true);
            if !mr.header_in.is_null() && std::ptr::eq(*bufp, mr.header_in) {
                break;
            }
        }
    }

    if data.len() > MAX_COPY_BYTES {
        return Err(raw_header_error("exceeds the 65536-byte copy limit"));
    }
    strip_preread_body(&mut data);
    Ok(bytes_as_string(data))
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
        let (args, truncated) = super::codec::decode_query_args(&bytes, max);
        ret(result_map("args", args, truncated))
    })
}

/// POST args from a completed request body (memory or bounded temp file).
#[pd_host_function(name = "ngx::req::get_post_args")]
pub fn get_post_args(max_args: &Value) -> VmResult<CallOutcome> {
    let max = clamp_count(
        max_args,
        "ngx::req::get_post_args max_args",
        DEFAULT_MAX_ARGS,
        HARD_MAX_ARGS,
    )?;
    with_request(|request| {
        if super::body::body_was_discarded(request.as_ref()) {
            return Err(VmError::HostError(
                "ngx::req::get_post_args request body was discarded".to_string(),
            ));
        }
        let bytes = super::body::copy_completed_body(request.as_ref(), "ngx::req::get_post_args")?;
        let (args, truncated) = super::codec::decode_query_args(&bytes, max);
        ret(result_map("args", args, truncated))
    })
}

/// True when NGINX marked the request internal.
#[pd_host_function(name = "ngx::req::is_internal")]
pub fn is_internal() -> VmResult<CallOutcome> {
    with_request(|request| {
        ret(request_flag(
            request.as_ref(),
            ngx_rustscript_request_is_internal,
        ))
    })
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
    with_request(|request| {
        ret(request_flag(
            request.as_ref(),
            ngx_rustscript_request_headers_sent,
        ))
    })
}

fn method_spec(method_id: i64) -> Option<(&'static [u8], ngx_uint_t)> {
    let id = u32::try_from(method_id).ok()?;
    const METHODS: &[(u32, &[u8])] = &[
        (NGX_HTTP_GET, b"GET"),
        (NGX_HTTP_HEAD, b"HEAD"),
        (NGX_HTTP_POST, b"POST"),
        (NGX_HTTP_PUT, b"PUT"),
        (NGX_HTTP_DELETE, b"DELETE"),
        (NGX_HTTP_MKCOL, b"MKCOL"),
        (NGX_HTTP_COPY, b"COPY"),
        (NGX_HTTP_MOVE, b"MOVE"),
        (NGX_HTTP_OPTIONS, b"OPTIONS"),
        (NGX_HTTP_PROPFIND, b"PROPFIND"),
        (NGX_HTTP_PROPPATCH, b"PROPPATCH"),
        (NGX_HTTP_LOCK, b"LOCK"),
        (NGX_HTTP_UNLOCK, b"UNLOCK"),
        (NGX_HTTP_PATCH, b"PATCH"),
        (NGX_HTTP_TRACE, b"TRACE"),
    ];
    METHODS
        .iter()
        .find(|(candidate, _)| *candidate == id)
        .map(|(_, name)| (*name, id as ngx_uint_t))
}

fn pool_copy(pool: *mut ngx_pool_t, bytes: &[u8], nul: bool) -> VmResult<*mut u8> {
    if pool.is_null() {
        return Err(VmError::HostError("request pool is null".into()));
    }
    let size = bytes.len() + usize::from(nul);
    if size == 0 {
        return Ok(std::ptr::null_mut());
    }
    let ptr = unsafe { ngx_palloc(pool, size) }.cast::<u8>();
    if ptr.is_null() {
        return Err(VmError::HostError("out of memory".into()));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        if nul {
            *ptr.add(bytes.len()) = 0;
        }
    }
    Ok(ptr)
}

fn ngx_str_from_bytes(pool: *mut ngx_pool_t, bytes: &[u8], nul: bool) -> VmResult<ngx_str_t> {
    if bytes.len() > MAX_COPY_BYTES {
        return Err(VmError::HostError(format!(
            "value exceeds the {MAX_COPY_BYTES}-byte copy limit"
        )));
    }
    let data = pool_copy(pool, bytes, nul)?;
    Ok(ngx_str_t {
        len: bytes.len(),
        data,
    })
}

fn uri_has_unsafe_bytes(uri: &str) -> Option<u8> {
    uri.bytes().find(|&b| matches!(b, 0x00..=0x08 | 0x0A..=0x1F | 0x7F))
}

fn escape_args_controls(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for byte in src.bytes() {
        if byte <= 0x20 || byte == 0x7F {
            out.push_str(&format!("%{byte:02X}"));
        } else {
            out.push(char::from(byte));
        }
    }
    out
}

fn is_header_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            matches!(
                b,
                b'!'
                    | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
                    | b'0'..=b'9'
                    | b'A'..=b'Z'
                    | b'a'..=b'z'
            )
        })
}

fn header_value_safe(value: &str) -> bool {
    !value.bytes().any(|b| matches!(b, 0 | b'\r' | b'\n'))
}

fn header_hash(low: &[u8]) -> ngx_uint_t {
    let mut key: ngx_uint_t = 0;
    for &byte in low {
        key = key.wrapping_mul(31).wrapping_add(byte as ngx_uint_t);
    }
    key
}

fn names_equal_ignore_case(key: ngx_str_t, name: &str) -> bool {
    if key.len != name.len() || key.data.is_null() {
        return false;
    }
    let bytes = unsafe { std::slice::from_raw_parts(key.data, key.len) };
    bytes.eq_ignore_ascii_case(name.as_bytes())
}

fn for_each_header_mut(
    list: &mut ngx_list_t,
    mut f: impl FnMut(&mut ngx_table_elt_t) -> VmResult<()>,
) -> VmResult<()> {
    let mut part = &mut list.part as *mut ngx_list_part_t;
    let mut seen_parts = [std::ptr::null_mut(); MAX_LIST_PARTS];
    let mut part_count = 0usize;
    while !part.is_null() {
        if part_count >= MAX_LIST_PARTS {
            return Err(VmError::HostError(
                "header list exceeds the list-part safety cap".into(),
            ));
        }
        if seen_parts[..part_count]
            .iter()
            .any(|&seen| std::ptr::eq(seen, part))
        {
            return Err(VmError::HostError("malformed cyclic header list".into()));
        }
        seen_parts[part_count] = part;
        part_count += 1;
        let part_ref = unsafe { &mut *part };
        if part_ref.nelts > 0 {
            if part_ref.elts.is_null() {
                return Err(VmError::HostError("malformed null header elts".into()));
            }
            let elts = unsafe {
                std::slice::from_raw_parts_mut(
                    part_ref.elts.cast::<ngx_table_elt_t>(),
                    part_ref.nelts,
                )
            };
            for header in elts {
                f(header)?;
            }
        }
        part = part_ref.next;
    }
    Ok(())
}

fn clear_matching_headers_except(
    list: &mut ngx_list_t,
    name: &str,
    keep: &[*mut ngx_table_elt_t],
) -> VmResult<()> {
    for_each_header_mut(list, |header| {
        if header.hash != 0 && names_equal_ignore_case(header.key, name) {
            let ptr = header as *mut ngx_table_elt_t;
            if !keep.iter().any(|&kept| std::ptr::eq(kept, ptr)) {
                header.hash = 0;
                header.next = std::ptr::null_mut();
            }
        }
        Ok(())
    })
}

fn push_header(
    list: *mut ngx_list_t,
    pool: *mut ngx_pool_t,
    name: &str,
    value: &str,
) -> VmResult<*mut ngx_table_elt_t> {
    if list.is_null() {
        return Err(VmError::HostError("header list is null".into()));
    }
    let low = lowercase_owned(name.as_bytes());
    let key = ngx_str_from_bytes(pool, name.as_bytes(), true)?;
    let lowcase = pool_copy(pool, &low, true)?;
    let val = ngx_str_from_bytes(pool, value.as_bytes(), true)?;
    let header = unsafe { ngx_list_push(list) }.cast::<ngx_table_elt_t>();
    if header.is_null() {
        return Err(VmError::HostError("out of memory".into()));
    }
    unsafe {
        (*header).hash = 0;
        (*header).key = key;
        (*header).value = val;
        (*header).lowcase_key = lowcase;
        (*header).next = std::ptr::null_mut();
        (*header).hash = header_hash(&low);
    }
    Ok(header)
}

fn parse_header_values(value: &Value) -> VmResult<Option<Vec<String>>> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) => {
            if !header_value_safe(text) {
                return Err(VmError::HostError(
                    "header value must not contain NUL, CR, or LF".into(),
                ));
            }
            if text.len() > MAX_HEADER_FIELD_BYTES {
                return Err(VmError::HostError(format!(
                    "header value exceeds {MAX_HEADER_FIELD_BYTES} bytes"
                )));
            }
            Ok(Some(vec![text.to_string()]))
        }
        Value::Array(items) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items.iter() {
                let Value::String(text) = item else {
                    return Err(VmError::HostError(
                        "header array values must be strings".into(),
                    ));
                };
                if !header_value_safe(text) {
                    return Err(VmError::HostError(
                        "header value must not contain NUL, CR, or LF".into(),
                    ));
                }
                if text.len() > MAX_HEADER_FIELD_BYTES {
                    return Err(VmError::HostError(format!(
                        "header value exceeds {MAX_HEADER_FIELD_BYTES} bytes"
                    )));
                }
                values.push(text.to_string());
            }
            Ok(Some(values))
        }
        _ => Err(VmError::HostError(
            "header value expects string, array, or null".into(),
        )),
    }
}

fn validate_header_name(name: &str) -> VmResult<()> {
    if name.len() > 256 {
        return Err(VmError::HostError("header name exceeds 256 bytes".into()));
    }
    if !is_header_token(name) {
        return Err(VmError::HostError(
            "header name is not a valid HTTP token".into(),
        ));
    }
    Ok(())
}

fn apply_request_specials(
    raw: &mut ngx_http_request_t,
    name: &str,
    header: *mut ngx_table_elt_t,
    host_server: Option<(ngx_str_t, bool)>,
) {
    match name.to_ascii_lowercase().as_str() {
        "host" => {
            raw.headers_in.host = header;
            if header.is_null() {
                raw.headers_in.server = ngx_str_t {
                    len: 0,
                    data: std::ptr::null_mut(),
                };
            } else if let Some((server, allocated)) = host_server {
                if allocated {
                    raw.headers_in.server = server;
                } else {
                    raw.headers_in.server = ngx_str_t {
                        len: server.len,
                        data: unsafe { (*header).value.data },
                    };
                }
            }
            unsafe {
                ngx_rustscript_c_invalidate_cached_host(raw);
            }
        }
        "connection" => unsafe {
            ngx_rustscript_c_headers_in_apply_connection(raw, header);
        },
        "user-agent" => unsafe {
            ngx_rustscript_c_headers_in_apply_user_agent(raw, header);
        },
        "content-length" => {
            raw.headers_in.content_length = header;
            if header.is_null() {
                raw.headers_in.content_length_n = -1;
            } else {
                let value = unsafe { (*header).value };
                let mut n: off_t = -1;
                let rc = unsafe {
                    ngx_rustscript_c_parse_content_length(value.data, value.len, &mut n)
                };
                raw.headers_in.content_length_n = if rc == 0 { n } else { -1 };
            }
        }
        "content-type" => raw.headers_in.content_type = header,
        "cookie" => raw.headers_in.cookie = header,
        "referer" => raw.headers_in.referer = header,
        _ => {}
    }
}

fn prevalidate_request_specials(
    raw: &ngx_http_request_t,
    name: &str,
    values: &[String],
) -> VmResult<Option<(ngx_str_t, bool)>> {
    match name.to_ascii_lowercase().as_str() {
        "host" => {
            let mut first = None;
            for (i, value) in values.iter().enumerate() {
                let mut server = ngx_str_t {
                    len: 0,
                    data: std::ptr::null_mut(),
                };
                let rust_ptr = value.as_ptr().cast_mut();
                let rc = unsafe {
                    ngx_rustscript_c_validate_host(raw.pool, rust_ptr, value.len(), &mut server)
                };
                if rc != 0 {
                    return Err(VmError::HostError("invalid Host header".into()));
                }
                if i == 0 {
                    let allocated =
                        !server.data.is_null() && !std::ptr::eq(server.data, rust_ptr);
                    first = Some((server, allocated));
                }
            }
            Ok(first)
        }
        "content-length" => {
            for value in values {
                let mut n: off_t = 0;
                let rc = unsafe {
                    ngx_rustscript_c_parse_content_length(value.as_ptr(), value.len(), &mut n)
                };
                if rc != 0 {
                    return Err(VmError::HostError("invalid Content-Length".into()));
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn validate_header_list(list: &ngx_list_t) -> VmResult<()> {
    let mut part = std::ptr::addr_of!(list.part);
    let mut seen = 0usize;
    while !part.is_null() {
        seen += 1;
        if seen > MAX_LIST_PARTS {
            return Err(VmError::HostError("malformed header list".into()));
        }
        let p = unsafe { &*part };
        if p.nelts > 0 && p.elts.is_null() {
            return Err(VmError::HostError("malformed header list".into()));
        }
        part = p.next;
    }
    Ok(())
}

fn set_or_clear_request_header(
    raw: &mut ngx_http_request_t,
    name: &str,
    values: Option<Vec<String>>,
) -> VmResult<()> {
    validate_header_name(name)?;
    validate_header_list(&raw.headers_in.headers)?;
    let Some(values) = values.filter(|values| !values.is_empty()) else {
        clear_matching_headers_except(&mut raw.headers_in.headers, name, &[])?;
        apply_request_specials(raw, name, std::ptr::null_mut(), None);
        return Ok(());
    };
    let host_server = prevalidate_request_specials(raw, name, &values)?;
    let pool = raw.pool;
    let list = &mut raw.headers_in.headers as *mut ngx_list_t;
    let mut added = Vec::new();
    let mut first: *mut ngx_table_elt_t = std::ptr::null_mut();
    let mut prev: *mut ngx_table_elt_t = std::ptr::null_mut();
    for value in &values {
        match push_header(list, pool, name, value) {
            Ok(header) => {
                if first.is_null() {
                    first = header;
                }
                if !prev.is_null() {
                    unsafe {
                        (*prev).next = header;
                    }
                }
                added.push(header);
                prev = header;
            }
            Err(err) => {
                for header in &added {
                    if !header.is_null() {
                        unsafe {
                            (**header).hash = 0;
                            (**header).next = std::ptr::null_mut();
                        }
                    }
                }
                return Err(err);
            }
        }
    }
    clear_matching_headers_except(&mut raw.headers_in.headers, name, &added)?;
    apply_request_specials(raw, name, first, host_server);
    Ok(())
}

/// Override the current request method using OpenResty method IDs.
#[pd_host_function(name = "ngx::req::set_method")]
pub fn set_method(method_id: i64) -> VmResult<CallOutcome> {
    let (name, id) = method_spec(method_id).ok_or_else(|| {
        VmError::HostError(format!("unsupported method id {method_id}"))
    })?;
    with_request_write(|request| {
        let raw = request.as_mut();
        let data = pool_copy(raw.pool, name, false)?;
        raw.method = id;
        raw.method_name = ngx_str_t {
            len: name.len(),
            data,
        };
        ret(())
    })
}

/// Rewrite the parsed URI. First release rejects `jump=true` before mutation.
#[pd_host_function(name = "ngx::req::set_uri")]
pub fn set_uri(uri: String, jump: &Value) -> VmResult<CallOutcome> {
    if uri.is_empty() {
        return Err(VmError::HostError("uri cannot be empty".into()));
    }
    if uri.len() > MAX_COPY_BYTES {
        return Err(VmError::HostError(format!(
            "uri exceeds the {MAX_COPY_BYTES}-byte copy limit"
        )));
    }
    if let Some(byte) = uri_has_unsafe_bytes(&uri) {
        return Err(VmError::HostError(format!(
            "unsafe byte \"0x{byte:02x}\" in uri"
        )));
    }
    if optional_bool(jump, "ngx::req::set_uri jump")?.unwrap_or(false) {
        return Err(VmError::HostError(
            "ngx.req.set_uri jump is not supported in this release".into(),
        ));
    }
    with_request_write(|request| {
        let raw = request.as_mut();
        raw.uri = ngx_str_from_bytes(raw.pool, uri.as_bytes(), false)?;
        unsafe {
            ngx_rustscript_c_request_set_uri_flags(raw, 1, 0, 0, 0);
            ngx_rustscript_c_request_set_exten(raw);
        }
        ret(())
    })
}

/// Replace the query string from a raw string or map.
#[pd_host_function(name = "ngx::req::set_uri_args")]
pub fn set_uri_args(args: &Value) -> VmResult<CallOutcome> {
    let encoded = match args {
        Value::String(text) => {
            if text.len() > MAX_COPY_BYTES {
                return Err(VmError::HostError(format!(
                    "uri args exceed the {MAX_COPY_BYTES}-byte copy limit"
                )));
            }
            escape_args_controls(text)
        }
        Value::Map(map) => super::codec::encode_args_map(map)?,
        _ => {
            return Err(VmError::HostError(
                "ngx::req::set_uri_args expects string or map".into(),
            ));
        }
    };
    if encoded.len() > MAX_COPY_BYTES {
        return Err(VmError::HostError(format!(
            "uri args exceed the {MAX_COPY_BYTES}-byte copy limit"
        )));
    }
    with_request_write(|request| {
        let raw = request.as_mut();
        raw.args = ngx_str_from_bytes(raw.pool, encoded.as_bytes(), false)?;
        unsafe {
            ngx_rustscript_c_request_set_valid_unparsed_uri(raw, 0);
        }
        ret(())
    })
}

/// Set or replace a request header. `null` clears.
#[pd_host_function(name = "ngx::req::set_header")]
pub fn set_header(name: String, value: &Value) -> VmResult<CallOutcome> {
    let values = parse_header_values(value)?;
    with_request_write(|request| {
        set_or_clear_request_header(request.as_mut(), &name, values.clone())?;
        ret(())
    })
}

/// Clear a request header, including dedicated special-field pointers.
#[pd_host_function(name = "ngx::req::clear_header")]
pub fn clear_header(name: String) -> VmResult<CallOutcome> {
    with_request_write(|request| {
        set_or_clear_request_header(request.as_mut(), &name, None)?;
        ret(())
    })
}
