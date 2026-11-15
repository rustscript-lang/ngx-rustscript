// Response metadata and control-flow hosts: status, headers_out, exit, redirect.

use std::os::raw::c_uint;

use nginx_sys::{
    ngx_http_request_t, ngx_list_part_t, ngx_list_push, ngx_list_t, ngx_palloc, ngx_pool_t,
    ngx_str_t, ngx_table_elt_t, ngx_uint_t,
};
use ngx::http::Request;
use pd_host_function::pd_host_function;
use vm::HostModuleDescriptor;
use vm::{Value, VmError, VmMap};

use super::super::VmResult;
use super::{CallOutcome, IntoHostCallOutcome, option_some, option_value};
use crate::host::{
    control_stop_error, current_identity, with_current_disposition_mut,
    with_current_request_and_disposition_mut, with_current_request_mut_ops,
    with_current_request_ops, AllowedOps, RequestDisposition, ScopeError,
};

const MAX_COPY_BYTES: usize = 65_536;
const MAX_HEADER_FIELD_BYTES: usize = 8_192;
const MAX_LIST_PARTS: usize = 64;

fn ret<T: IntoHostCallOutcome>(value: T) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn scope_err(err: ScopeError) -> VmError {
    VmError::HostError(err.to_string())
}

fn with_response_read<T>(f: impl FnOnce(&Request) -> VmResult<T>) -> VmResult<T> {
    let identity = current_identity().map_err(scope_err)?;
    with_current_request_ops(identity, AllowedOps::RESPONSE_READ, f).map_err(scope_err)?
}

fn with_response_write<T>(f: impl FnOnce(&mut Request) -> VmResult<T>) -> VmResult<T> {
    let identity = current_identity().map_err(scope_err)?;
    with_current_request_mut_ops(identity, AllowedOps::RESPONSE_WRITE, f).map_err(scope_err)?
}

unsafe extern "C" {
    fn ngx_rustscript_c_request_headers_sent(r: *const ngx_http_request_t) -> c_uint;
    fn ngx_rustscript_c_headers_out_reset_specials(r: *mut ngx_http_request_t);
    fn ngx_rustscript_c_parse_http_time(data: *const u8, len: usize) -> i64;
    fn ngx_rustscript_c_resp_status(r: *const ngx_http_request_t) -> nginx_sys::ngx_int_t;
}

fn headers_sent(raw: &ngx_http_request_t) -> bool {
    unsafe { ngx_rustscript_c_request_headers_sent(raw) != 0 }
}

fn reject_if_headers_sent(raw: &ngx_http_request_t) -> VmResult<()> {
    if headers_sent(raw) {
        Err(VmError::HostError("attempted to set headers after they were sent".into()))
    } else {
        Ok(())
    }
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

fn deactivate_header(header: *mut ngx_table_elt_t) {
    if header.is_null() {
        return;
    }
    unsafe {
        (*header).hash = 0;
        (*header).next = std::ptr::null_mut();
    }
}

fn rollback_added_headers(added: &[*mut ngx_table_elt_t]) {
    for header in added {
        deactivate_header(*header);
    }
}

fn collect_active_headers(list: &mut ngx_list_t) -> VmResult<Vec<*mut ngx_table_elt_t>> {
    let mut out = Vec::new();
    for_each_header_mut(list, |header| {
        if header.hash != 0 {
            out.push(header as *mut ngx_table_elt_t);
        }
        Ok(())
    })?;
    Ok(out)
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

fn clear_all_headers(list: &mut ngx_list_t) -> VmResult<()> {
    for_each_header_mut(list, |header| {
        header.hash = 0;
        header.next = std::ptr::null_mut();
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
    let low: Vec<u8> = name.bytes().map(|b| b.to_ascii_lowercase()).collect();
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

fn append_headers(
    list: *mut ngx_list_t,
    pool: *mut ngx_pool_t,
    name: &str,
    values: &[String],
) -> VmResult<Vec<*mut ngx_table_elt_t>> {
    let mut added = Vec::new();
    let mut prev: *mut ngx_table_elt_t = std::ptr::null_mut();
    for value in values {
        match push_header(list, pool, name, value) {
            Ok(header) => {
                if !prev.is_null() {
                    unsafe {
                        (*prev).next = header;
                    }
                }
                added.push(header);
                prev = header;
            }
            Err(err) => {
                rollback_added_headers(&added);
                return Err(err);
            }
        }
    }
    Ok(added)
}

fn apply_response_specials(raw: &mut ngx_http_request_t, name: &str, header: *mut ngx_table_elt_t) {
    match name.to_ascii_lowercase().as_str() {
        "content-type" => {
            if header.is_null() {
                raw.headers_out.content_type_len = 0;
                raw.headers_out.content_type = ngx_str_t {
                    len: 0,
                    data: std::ptr::null_mut(),
                };
                raw.headers_out.content_type_lowcase = std::ptr::null_mut();
            } else {
                let value = unsafe { (*header).value };
                let type_len = if value.data.is_null() {
                    0
                } else {
                    let bytes = unsafe { std::slice::from_raw_parts(value.data, value.len) };
                    bytes
                        .iter()
                        .position(|&b| b == b';')
                        .unwrap_or(value.len)
                };
                raw.headers_out.content_type_len = type_len;
                raw.headers_out.content_type = value;
                raw.headers_out.content_type_lowcase = std::ptr::null_mut();
                // Dedicated Content-Type is emitted by the header filter; keep
                // the list slot inactive so the name is not written twice.
                unsafe {
                    (*header).hash = 0;
                }
            }
        }
        "content-length" => {
            raw.headers_out.content_length = header;
            raw.headers_out.content_length_n = if header.is_null() {
                -1
            } else {
                let value = unsafe { (*header).value };
                let bytes = if value.data.is_null() {
                    &[][..]
                } else {
                    unsafe { std::slice::from_raw_parts(value.data, value.len) }
                };
                std::str::from_utf8(bytes)
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(-1)
            };
        }
        "location" => {
            if header.is_null() {
                raw.headers_out.location = std::ptr::null_mut();
            } else {
                let value = unsafe { (*header).value };
                let starts_slash = value.len > 0
                    && !value.data.is_null()
                    && unsafe { *value.data } == b'/';
                raw.headers_out.location = if starts_slash {
                    std::ptr::null_mut()
                } else {
                    header
                };
            }
        }
        "last-modified" => {
            raw.headers_out.last_modified = header;
            raw.headers_out.last_modified_time = if header.is_null() {
                -1
            } else {
                let value = unsafe { (*header).value };
                unsafe { ngx_rustscript_c_parse_http_time(value.data, value.len) }
            };
        }
        "etag" => raw.headers_out.etag = header,
        _ => {}
    }
}

fn reset_response_specials(raw: &mut ngx_http_request_t) {
    unsafe {
        ngx_rustscript_c_headers_out_reset_specials(raw as *mut ngx_http_request_t);
    }
}

fn set_or_clear_response_header(
    raw: &mut ngx_http_request_t,
    name: &str,
    values: Option<Vec<String>>,
) -> VmResult<()> {
    validate_header_name(name)?;
    reject_if_headers_sent(raw)?;
    validate_header_list(&raw.headers_out.headers)?;
    let Some(values) = values.filter(|values| !values.is_empty()) else {
        clear_matching_headers_except(&mut raw.headers_out.headers, name, &[])?;
        apply_response_specials(raw, name, std::ptr::null_mut());
        return Ok(());
    };
    let pool = raw.pool;
    let list = &mut raw.headers_out.headers as *mut ngx_list_t;
    let added = append_headers(list, pool, name, &values)?;
    clear_matching_headers_except(&mut raw.headers_out.headers, name, &added)?;
    apply_response_specials(
        raw,
        name,
        added.first().copied().unwrap_or(std::ptr::null_mut()),
    );
    Ok(())
}

fn ngx_str_to_value(value: ngx_str_t) -> VmResult<Value> {
    if value.len > 0 && value.data.is_null() {
        return Err(VmError::HostError("malformed header value".into()));
    }
    let bytes = if value.data.is_null() || value.len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(value.data, value.len) }.to_vec()
    };
    Ok(Value::string(match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => err.into_bytes().into_iter().map(char::from).collect(),
    }))
}

fn dedicated_response_header(raw: &ngx_http_request_t, name: &str) -> VmResult<Option<Value>> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "content-type" if raw.headers_out.content_type.len > 0 => {
            Some(ngx_str_to_value(raw.headers_out.content_type)?)
        }
        "content-length" if raw.headers_out.content_length_n >= 0 => {
            Some(Value::string(raw.headers_out.content_length_n.to_string()))
        }
        _ => None,
    })
}

/// First matching response header value as an owned string.
fn collect_named_header(list: &ngx_list_t, name: &str) -> VmResult<Option<String>> {
    let mut values = Vec::new();
    let mut part = &list.part as *const ngx_list_part_t;
    let mut seen_parts = [std::ptr::null(); MAX_LIST_PARTS];
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
        let part_ref = unsafe { &*part };
        if part_ref.nelts > 0 {
            if part_ref.elts.is_null() {
                return Err(VmError::HostError("malformed null header elts".into()));
            }
            let elts = unsafe {
                std::slice::from_raw_parts(part_ref.elts.cast::<ngx_table_elt_t>(), part_ref.nelts)
            };
            for header in elts {
                if header.hash == 0 || !names_equal_ignore_case(header.key, name) {
                    continue;
                }
                let Value::String(text) = ngx_str_to_value(header.value)? else {
                    return Err(VmError::HostError(
                        "response header value is not a string".into(),
                    ));
                };
                values.push(text);
            }
        }
        part = part_ref.next;
    }
    Ok(values.into_iter().next().map(|text| (*text).clone()))
}

fn uri_has_unsafe_bytes(uri: &str) -> Option<u8> {
    uri.bytes()
        .find(|&b| matches!(b, 0x00..=0x08 | 0x0A..=0x1F | 0x7F))
}

fn stop_with_disposition(disp: RequestDisposition) -> VmResult<CallOutcome> {
    let identity = current_identity().map_err(scope_err)?;
    with_current_disposition_mut(identity, |slot| {
        if *slot == RequestDisposition::Continue {
            *slot = disp;
        }
    })
    .map_err(scope_err)?;
    Err(control_stop_error())
}

fn allowed_exit_status(status: i64) -> bool {
    matches!(status, 0 | -1 | -5) || (100..=599).contains(&status)
}

fn allowed_redirect_status(status: i64) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Current response status. Prefers `err_status`, then `headers_out.status`,
/// then HTTP/0.9 maps to `9`. Unset HTTP/1.x is `0`.
#[pd_host_function(name = "ngx::status::get", contract = crate::host::catalog::ngx_status_get_contract)]
pub fn status_get() -> VmResult<CallOutcome> {
    with_response_read(|request| {
        let status = unsafe { ngx_rustscript_c_resp_status(request.as_ref()) };
        ret(status as i64)
    })
}

/// Set `headers_out.status`. Rejected after headers are sent.
#[pd_host_function(name = "ngx::status::set", contract = crate::host::catalog::ngx_status_set_contract)]
pub fn status_set(status: i64) -> VmResult<CallOutcome> {
    if !(0..=999).contains(&status) {
        return Err(VmError::HostError("invalid response status".into()));
    }
    with_response_write(|request| {
        let raw = request.as_mut();
        reject_if_headers_sent(raw)?;
        raw.headers_out.status = status as ngx_uint_t;
        ret(())
    })
}

/// Read one response header. Missing is `Option::None`; duplicate names yield
/// the first value (use `ngx::req::get_headers` for the full multi-value set).
#[pd_host_function(name = "ngx::header::get", contract = crate::host::catalog::ngx_header_get_contract)]
pub fn header_get(name: String) -> VmResult<CallOutcome> {
    validate_header_name(&name)?;
    with_response_read(|request| {
        let raw = request.as_ref();
        if let Some(value) = dedicated_response_header(raw, &name)? {
            return ret(option_some(value));
        }
        ret(option_value(
            collect_named_header(&raw.headers_out.headers, &name)?
                .map(Value::string),
        ))
    })
}

/// Set or replace a response header. `null` clears.
#[pd_host_function(name = "ngx::header::set", contract = crate::host::catalog::ngx_header_set_contract)]
pub fn header_set(name: String, value: &Value) -> VmResult<CallOutcome> {
    let values = parse_header_values(value)?;
    with_response_write(|request| {
        set_or_clear_response_header(request.as_mut(), &name, values.clone())?;
        ret(())
    })
}

/// Atomically replace every response header from a map.
#[pd_host_function(name = "ngx::header::set_all", contract = crate::host::catalog::ngx_header_set_all_contract)]
pub fn header_set_all(headers: &VmMap) -> VmResult<CallOutcome> {
    let mut prepared = Vec::new();
    for (key, value) in headers.iter() {
        let Value::String(name) = key else {
            return Err(VmError::HostError(
                "ngx::header::set_all keys must be strings".into(),
            ));
        };
        validate_header_name(name)?;
        let values = parse_header_values(value)?;
        prepared.push((name.to_string(), values));
    }
    with_response_write(|request| {
        let raw = request.as_mut();
        reject_if_headers_sent(raw)?;
        validate_header_list(&raw.headers_out.headers)?;
        let old = collect_active_headers(&mut raw.headers_out.headers)?;
        let mut added = Vec::new();
        let mut specials = Vec::new();
        for (name, values) in &prepared {
            let Some(values) = values.as_ref().filter(|values| !values.is_empty()) else {
                continue;
            };
            match append_headers(
                &mut raw.headers_out.headers as *mut ngx_list_t,
                raw.pool,
                name,
                values,
            ) {
                Ok(headers) => {
                    if let Some(first) = headers.first().copied() {
                        specials.push((name.clone(), first));
                    }
                    added.extend(headers);
                }
                Err(err) => {
                    rollback_added_headers(&added);
                    return Err(err);
                }
            }
        }
        for header in old {
            if !added.iter().any(|&kept| std::ptr::eq(kept, header)) {
                deactivate_header(header);
            }
        }
        reset_response_specials(raw);
        for (name, header) in specials {
            apply_response_specials(raw, &name, header);
        }
        ret(())
    })
}

/// Clear every response header and dedicated special fields.
#[pd_host_function(name = "ngx::header::clear", contract = crate::host::catalog::ngx_header_clear_contract)]
pub fn header_clear() -> VmResult<CallOutcome> {
    with_response_write(|request| {
        let raw = request.as_mut();
        reject_if_headers_sent(raw)?;
        clear_all_headers(&mut raw.headers_out.headers)?;
        reset_response_specials(raw);
        ret(())
    })
}

/// Stop the VM and finalize with `status`. First disposition wins.
#[pd_host_function(name = "ngx::exit", contract = crate::host::catalog::ngx_exit_contract)]
pub fn exit(status: i64) -> VmResult<CallOutcome> {
    if !allowed_exit_status(status) {
        return Err(VmError::HostError(format!("invalid exit status {status}")));
    }
    stop_with_disposition(RequestDisposition::Finalize(status as i32))
}

/// Write Location and stop the VM with an external redirect.
#[pd_host_function(name = "ngx::redirect", contract = crate::host::catalog::ngx_redirect_contract)]
pub fn redirect(uri: String, status: &Value) -> VmResult<CallOutcome> {
    if uri.is_empty() {
        return Err(VmError::HostError("redirect uri cannot be empty".into()));
    }
    if uri.len() > MAX_COPY_BYTES {
        return Err(VmError::HostError(format!(
            "redirect uri exceeds the {MAX_COPY_BYTES}-byte copy limit"
        )));
    }
    if let Some(byte) = uri_has_unsafe_bytes(&uri) {
        return Err(VmError::HostError(format!(
            "unsafe byte \"0x{byte:02x}\" in redirect uri"
        )));
    }
    let code = match status {
        Value::Null => 302,
        Value::Int(n) if allowed_redirect_status(*n) => *n,
        Value::Int(n) => {
            return Err(VmError::HostError(format!("invalid redirect status {n}")));
        }
        _ => {
            return Err(VmError::HostError(
                "ngx::redirect status expects int or null".into(),
            ));
        }
    };
    let identity = current_identity().map_err(scope_err)?;
    with_current_request_and_disposition_mut(identity, |request, disp| {
        if *disp != RequestDisposition::Continue {
            return Ok::<(), VmError>(());
        }
        let raw = request.as_mut();
        reject_if_headers_sent(raw)?;
        set_or_clear_response_header(raw, "Location", Some(vec![uri.clone()]))?;
        raw.headers_out.status = code as ngx_uint_t;
        *disp = RequestDisposition::ExternalRedirect {
            uri: uri.clone(),
            status: code as i32,
        };
        Ok(())
    })
    .map_err(scope_err)??;
    Err(control_stop_error())
}


const RESPONSE_META_HOST_MODULE_FUNCTIONS: &[fn() -> vm::HostFunctionDescriptor] = &[
        status_get_descriptor,
        status_set_descriptor,
        header_get_descriptor,
        header_set_descriptor,
        header_set_all_descriptor,
        header_clear_descriptor,
        exit_descriptor,
        redirect_descriptor,
];

pub fn response_meta_host_module() -> HostModuleDescriptor {
    HostModuleDescriptor {
        name: "ngx.resp",
        functions: RESPONSE_META_HOST_MODULE_FUNCTIONS,
        resources: &[],
    }
}
