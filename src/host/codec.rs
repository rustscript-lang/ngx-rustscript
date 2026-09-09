// OpenResty-compatible codec and hash hosts.
//
// Optional OpenResty trailing arguments are explicit `null` in RustScript:
// the pinned compiler matches host arity exactly and does not treat
// `optional<T>` as compatible with `T` or `null` at a host call site.

use pd_host_function::pd_host_function;
use vm::{Value, VmError, VmMap};

use super::{CallOutcome, IntoHostCallOutcome};
use super::super::VmResult;

const MAX_CODEC_INPUT_BYTES: usize = 65_536;
const MAX_CODEC_OUTPUT_BYTES: usize = 196_608;
const DEFAULT_DECODE_ARGS: i64 = 100;
const HARD_MAX_DECODE_ARGS: i64 = 1_024;
const MAX_ENCODE_PAIRS: usize = 1_024;

fn ret<T: IntoHostCallOutcome>(value: T) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn limit_input(name: &str, len: usize) -> VmResult<()> {
    if len > MAX_CODEC_INPUT_BYTES {
        return Err(VmError::HostError(format!(
            "{name} input exceeds the {MAX_CODEC_INPUT_BYTES}-byte limit"
        )));
    }
    Ok(())
}

fn limit_output(name: &str, len: usize) -> VmResult<()> {
    if len > MAX_CODEC_OUTPUT_BYTES {
        return Err(VmError::HostError(format!(
            "{name} output exceeds the {MAX_CODEC_OUTPUT_BYTES}-byte limit"
        )));
    }
    Ok(())
}

fn input_bytes(value: &Value, name: &str) -> VmResult<Vec<u8>> {
    match value {
        Value::String(text) => {
            limit_input(name, text.len())?;
            Ok(text.as_bytes().to_vec())
        }
        Value::Bytes(bytes) => {
            limit_input(name, bytes.len())?;
            Ok(bytes.as_ref().clone())
        }
        _ => Err(VmError::HostError(format!(
            "{name} expects string or bytes"
        ))),
    }
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

fn utf8_string(name: &str, bytes: Vec<u8>) -> VmResult<String> {
    String::from_utf8(bytes).map_err(|_| {
        VmError::HostError(format!("{name} produced non-utf8 bytes"))
    })
}

fn bytes_as_string(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => err.into_bytes().into_iter().map(char::from).collect(),
    }
}

/// `ngx.escape_uri` type bitmaps from lua-nginx-module `ngx_http_lua_escape_uri`.
const ESCAPE_URI: [u32; 8] = [
    0xffff_ffff, 0x8000_0029, 0x0000_0000, 0x8000_0000, 0xffff_ffff, 0xffff_ffff,
    0xffff_ffff, 0xffff_ffff,
];
const ESCAPE_ARGS: [u32; 8] = [
    0xffff_ffff, 0x8000_0829, 0x0000_0000, 0x8000_0000, 0xffff_ffff, 0xffff_ffff,
    0xffff_ffff, 0xffff_ffff,
];
const ESCAPE_URI_COMPONENT: [u32; 8] = [
    0xffff_ffff, 0xfc00_987d, 0x7800_0001, 0xb800_0001, 0xffff_ffff, 0xffff_ffff,
    0xffff_ffff, 0xffff_ffff,
];
const ESCAPE_HTML: [u32; 8] = [
    0xffff_ffff, 0x0000_00ad, 0x0000_0000, 0x8000_0000, 0xffff_ffff, 0xffff_ffff,
    0xffff_ffff, 0xffff_ffff,
];
const ESCAPE_REFRESH: [u32; 8] = [
    0xffff_ffff, 0x0000_0085, 0x0000_0000, 0x8000_0000, 0xffff_ffff, 0xffff_ffff,
    0xffff_ffff, 0xffff_ffff,
];
const ESCAPE_MEMCACHED: [u32; 8] = [
    0xffff_ffff, 0x0000_0021, 0x0000_0000, 0x0000_0000, 0x0000_0000, 0x0000_0000,
    0x0000_0000, 0x0000_0000,
];
const ESCAPE_HEADER_NAME: [u32; 8] = [
    0xffff_ffff, 0xfc00_9305, 0x3800_0001, 0xa800_0000, 0xffff_ffff, 0xffff_ffff,
    0xffff_ffff, 0xffff_ffff,
];
const ESCAPE_HEADER_VALUE: [u32; 8] = [
    0xffff_fdff, 0x0000_0000, 0x0000_0000, 0x8000_0000, 0x0000_0000, 0x0000_0000,
    0x0000_0000, 0x0000_0000,
];

fn escape_table(escape_type: i64) -> VmResult<&'static [u32; 8]> {
    Ok(match escape_type {
        0 => &ESCAPE_URI,
        1 => &ESCAPE_ARGS,
        2 => &ESCAPE_URI_COMPONENT,
        3 => &ESCAPE_HTML,
        4 => &ESCAPE_REFRESH,
        5 | 6 => &ESCAPE_MEMCACHED,
        7 => &ESCAPE_HEADER_NAME,
        8 => &ESCAPE_HEADER_VALUE,
        other => {
            return Err(VmError::HostError(format!(
                "ngx::escape_uri type {other} is not supported"
            )));
        }
    })
}

fn needs_escape(byte: u8, table: &[u32; 8]) -> bool {
    table[usize::from(byte >> 5)] & (1u32 << (byte & 0x1f)) != 0
}

fn escape_bytes(name: &str, src: &[u8], table: &[u32; 8]) -> VmResult<String> {
    let extra = src.iter().filter(|byte| needs_escape(**byte, table)).count();
    let out_len = src.len() + extra * 2;
    limit_output(name, out_len)?;
    let mut out = String::with_capacity(out_len);
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in src {
        if needs_escape(byte, table) {
            out.push('%');
            out.push(HEX[usize::from(byte >> 4)] as char);
            out.push(HEX[usize::from(byte & 0x0f)] as char);
        } else {
            out.push(byte as char);
        }
    }
    Ok(out)
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
            b'%' if index + 2 < src.len()
                && src[index + 1].is_ascii_hexdigit()
                && src[index + 2].is_ascii_hexdigit() =>
            {
                let hi = hex_nibble(src[index + 1]);
                let lo = hex_nibble(src[index + 2]);
                out.push((hi << 4) | lo);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    out
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'A'..=b'F' => byte - b'A' + 10,
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
    }
}

/// Escapes `uri` using OpenResty `ngx.escape_uri`.
///
/// `escape_type` is `null` (default component / type 2) or an integer 0..=8.
#[pd_host_function(name = "ngx::escape_uri")]
pub fn escape_uri(uri: &str, escape_type: &Value) -> VmResult<CallOutcome> {
    limit_input("ngx::escape_uri", uri.len())?;
    let escape_type = optional_int(escape_type, "ngx::escape_uri type")?.unwrap_or(2);
    let table = escape_table(escape_type)?;
    ret(escape_bytes("ngx::escape_uri", uri.as_bytes(), table)?)
}

/// Unescapes URI text. `+` becomes space; invalid `%` sequences are left as-is.
#[pd_host_function(name = "ngx::unescape_uri")]
pub fn unescape_uri(uri: &str) -> VmResult<CallOutcome> {
    limit_input("ngx::unescape_uri", uri.len())?;
    ret(bytes_as_string(unescape_bytes(uri.as_bytes())))
}

enum EncodedArg {
    Flag,
    Value(String),
}

fn format_arg_value(name: &str, value: &Value) -> VmResult<Option<EncodedArg>> {
    match value {
        Value::Null | Value::Bool(false) => Ok(None),
        Value::Bool(true) => Ok(Some(EncodedArg::Flag)),
        Value::Int(number) => Ok(Some(EncodedArg::Value(number.to_string()))),
        Value::Float(number) => {
            if number.is_nan() || number.is_infinite() {
                return Err(VmError::HostError(format!(
                    "{name} rejects non-finite numbers"
                )));
            }
            if number.fract() == 0.0 && number.abs() < 1e15 {
                Ok(Some(EncodedArg::Value((*number as i64).to_string())))
            } else {
                Ok(Some(EncodedArg::Value(number.to_string())))
            }
        }
        Value::String(text) => {
            limit_input(name, text.len())?;
            Ok(Some(EncodedArg::Value(text.as_str().to_string())))
        }
        Value::Bytes(bytes) => {
            limit_input(name, bytes.len())?;
            Ok(Some(EncodedArg::Value(utf8_string(
                name,
                bytes.as_ref().clone(),
            )?)))
        }
        Value::Array(_) | Value::Map(_) | Value::Callable(_) => Err(VmError::HostError(
            format!("{name} cannot encode nested tables as a single value"),
        )),
    }
}

fn push_encoded_pair(
    pairs: &mut Vec<(String, Option<String>)>,
    key: &str,
    encoded: EncodedArg,
) -> VmResult<()> {
    if pairs.len() >= MAX_ENCODE_PAIRS {
        return Err(VmError::HostError(format!(
            "ngx::encode_args exceeds {MAX_ENCODE_PAIRS} pairs"
        )));
    }
    match encoded {
        EncodedArg::Flag => pairs.push((key.to_string(), None)),
        EncodedArg::Value(value) => pairs.push((key.to_string(), Some(value))),
    }
    Ok(())
}

/// Encodes a map as an application/x-www-form-urlencoded string.
///
/// Catalog type is `unknown` because RustScript object literals do not type as
/// `map<unknown>`. Array values emit repeated keys, matching OpenResty table
/// values. VmMap duplicate-key inserts overwrite; callers must use arrays for
/// multi-value keys.
#[pd_host_function(name = "ngx::encode_args")]
pub fn encode_args(args: &VmMap) -> VmResult<CallOutcome> {
    ret(encode_args_map(args)?)
}

fn encode_args_map(args: &VmMap) -> VmResult<String> {
    let mut pairs: Vec<(String, Option<String>)> = Vec::new();
    for (key, value) in args.iter() {
        let Value::String(key) = key else {
            return Err(VmError::HostError(
                "ngx::encode_args keys must be strings".to_string(),
            ));
        };
        limit_input("ngx::encode_args", key.len())?;
        match value {
            Value::Array(values) => {
                if values.is_empty() {
                    continue;
                }
                for item in values.iter() {
                    if let Some(encoded) = format_arg_value("ngx::encode_args", item)? {
                        push_encoded_pair(&mut pairs, key, encoded)?;
                    }
                }
            }
            other => {
                if let Some(encoded) = format_arg_value("ngx::encode_args", other)? {
                    push_encoded_pair(&mut pairs, key, encoded)?;
                }
            }
        }
    }

    let table = escape_table(2)?;
    let mut out = String::new();
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(&escape_bytes("ngx::encode_args", key.as_bytes(), table)?);
        if let Some(value) = value {
            out.push('=');
            out.push_str(&escape_bytes("ngx::encode_args", value.as_bytes(), table)?);
        }
        limit_output("ngx::encode_args", out.len())?;
    }
    Ok(out)
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

pub(crate) fn decode_query_args(query: &[u8], max_args: usize) -> (VmMap, bool) {
    let mut args = VmMap::new();
    let mut seen = 0usize;
    let mut truncated = false;
    if query.is_empty() {
        return (args, false);
    }
    for part in query.split(|byte| *byte == b'&') {
        if part.is_empty() {
            continue;
        }
        if seen >= max_args {
            truncated = true;
            break;
        }
        let (raw_key, value) = match part.iter().position(|byte| *byte == b'=') {
            Some(idx) => (
                &part[..idx],
                Value::string(bytes_as_string(unescape_bytes(&part[idx + 1..]))),
            ),
            None => (part, Value::Bool(true)),
        };
        let key = bytes_as_string(unescape_bytes(raw_key));
        if key.is_empty() {
            continue;
        }
        insert_decoded_arg(&mut args, key, value);
        seen += 1;
    }
    (args, truncated)
}

/// Decodes an application/x-www-form-urlencoded string.
///
/// Returns `{ args, truncated }`. `max_args` is `null` (default 100), a
/// positive cap, or `0` which is clamped to the hard limit 1024 rather than
/// becoming unbounded.
#[pd_host_function(name = "ngx::decode_args")]
pub fn decode_args(query: &str, max_args: &Value) -> VmResult<CallOutcome> {
    limit_input("ngx::decode_args", query.len())?;
    let max_args = match optional_int(max_args, "ngx::decode_args max_args")? {
        None => DEFAULT_DECODE_ARGS,
        Some(0) => HARD_MAX_DECODE_ARGS,
        Some(value) if value < 0 => {
            return Err(VmError::HostError(
                "ngx::decode_args max_args must be non-negative".to_string(),
            ));
        }
        Some(value) if value > HARD_MAX_DECODE_ARGS => {
            return Err(VmError::HostError(format!(
                "ngx::decode_args max_args exceeds {HARD_MAX_DECODE_ARGS}"
            )));
        }
        Some(value) => value,
    } as usize;

    let (args, truncated) = decode_query_args(query.as_bytes(), max_args);
    let mut result = VmMap::new();
    result.insert(Value::string("args"), Value::Map(std::sync::Arc::new(args)));
    result.insert(Value::string("truncated"), Value::Bool(truncated));
    ret(result)
}

fn encode_base64_bytes(bytes: &[u8], no_padding: bool) -> String {
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    if no_padding {
        encoded.trim_end_matches('=').to_string()
    } else {
        encoded
    }
}

fn decode_base64_bytes(text: &str) -> Option<Vec<u8>> {
    let mut padded = text.to_string();
    match padded.len() % 4 {
        0 => {}
        1 => return None,
        2 => padded.push_str("=="),
        3 => padded.push('='),
        _ => unreachable!(),
    }
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, padded.as_bytes()).ok()
}

fn is_base64_alphabet(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/'
}

/// OpenResty `ngx.decode_base64mime`: skip non-alphabet bytes and stop at the
/// first `=`. `data_len % 4 == 1` is invalid (`null`), matching
/// `ngx_http_lua_decode_base64mime`.
fn decode_base64_mime(text: &str) -> Option<Vec<u8>> {
    let mut alphabet = Vec::with_capacity(text.len());
    for &byte in text.as_bytes() {
        if byte == b'=' {
            break;
        }
        if is_base64_alphabet(byte) {
            alphabet.push(byte);
        }
    }
    decode_base64_bytes(std::str::from_utf8(&alphabet).ok()?)
}

/// Encodes string or bytes as standard Base64. `no_padding` is `null`/`false`
/// or `true` to drop `=` padding.
#[pd_host_function(name = "ngx::encode_base64")]
pub fn encode_base64(input: &Value, no_padding: &Value) -> VmResult<CallOutcome> {
    let bytes = input_bytes(input, "ngx::encode_base64")?;
    let no_padding = optional_bool(no_padding, "ngx::encode_base64 no_padding")?.unwrap_or(false);
    let encoded = encode_base64_bytes(&bytes, no_padding);
    limit_output("ngx::encode_base64", encoded.len())?;
    ret(encoded)
}

/// Decodes standard Base64. Padding may be omitted. Invalid input is `null`.
#[pd_host_function(name = "ngx::decode_base64")]
pub fn decode_base64(text: &str) -> VmResult<CallOutcome> {
    limit_input("ngx::decode_base64", text.len())?;
    ret(decode_base64_bytes(text))
}

/// MIME Base64 decode: non-alphabet bytes are skipped and decoding stops at
/// the first `=`, matching OpenResty `ngx.decode_base64mime`.
#[pd_host_function(name = "ngx::decode_base64mime")]
pub fn decode_base64mime(text: &str) -> VmResult<CallOutcome> {
    limit_input("ngx::decode_base64mime", text.len())?;
    ret(decode_base64_mime(text))
}

fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// CRC-32 of `data`. OpenResty `crc32_short` and `crc32_long` currently match.
#[pd_host_function(name = "ngx::crc32_short")]
pub fn crc32_short(data: &str) -> VmResult<CallOutcome> {
    limit_input("ngx::crc32_short", data.len())?;
    ret(i64::from(crc32_ieee(data.as_bytes())))
}

/// CRC-32 of `data`. Same result as [`crc32_short`].
#[pd_host_function(name = "ngx::crc32_long")]
pub fn crc32_long(data: &str) -> VmResult<CallOutcome> {
    limit_input("ngx::crc32_long", data.len())?;
    ret(i64::from(crc32_ieee(data.as_bytes())))
}

/// HMAC-SHA1 of `data` with `secret`. Returns raw bytes.
#[pd_host_function(name = "ngx::hmac_sha1")]
pub fn hmac_sha1(secret: &str, data: &str) -> VmResult<CallOutcome> {
    limit_input("ngx::hmac_sha1", secret.len())?;
    limit_input("ngx::hmac_sha1", data.len())?;
    use hmac::{Hmac, Mac};
    use sha1::Sha1;
    let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes())
        .map_err(|_| VmError::HostError("ngx::hmac_sha1 invalid key".to_string()))?;
    mac.update(data.as_bytes());
    ret(mac.finalize().into_bytes().to_vec())
}

fn md5_bytes(data: &[u8]) -> Vec<u8> {
    use md5::{Digest, Md5};
    Md5::digest(data).to_vec()
}

fn sha1_bytes(data: &[u8]) -> Vec<u8> {
    use sha1::{Digest, Sha1};
    Sha1::digest(data).to_vec()
}

fn to_hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// MD5 hex digest (lowercase), matching OpenResty `ngx.md5`.
#[pd_host_function(name = "ngx::md5")]
pub fn md5(data: &str) -> VmResult<CallOutcome> {
    limit_input("ngx::md5", data.len())?;
    ret(to_hex_lower(&md5_bytes(data.as_bytes())))
}

/// MD5 raw digest bytes.
#[pd_host_function(name = "ngx::md5_bin")]
pub fn md5_bin(data: &str) -> VmResult<CallOutcome> {
    limit_input("ngx::md5_bin", data.len())?;
    ret(md5_bytes(data.as_bytes()))
}

/// SHA-1 raw digest bytes.
#[pd_host_function(name = "ngx::sha1_bin")]
pub fn sha1_bin(data: &str) -> VmResult<CallOutcome> {
    limit_input("ngx::sha1_bin", data.len())?;
    ret(sha1_bytes(data.as_bytes()))
}

/// Quotes `value` as a MySQL string literal, matching `ngx.quote_sql_str`.
#[pd_host_function(name = "ngx::quote_sql_str")]
pub fn quote_sql_str(value: &str) -> VmResult<CallOutcome> {
    limit_input("ngx::quote_sql_str", value.len())?;
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        match ch {
            '\0' => out.push_str("\\0"),
            '\u{08}' => out.push_str("\\b"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{1a}' => out.push_str("\\Z"),
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
        limit_output("ngx::quote_sql_str", out.len())?;
    }
    out.push('\'');
    ret(out)
}

#[cfg(test)]
mod encode_pair_cap {
    use super::*;

    fn array_map(len: usize) -> VmMap {
        let mut map = VmMap::new();
        let values: Vec<Value> = (0..len).map(|i| Value::Int(i as i64)).collect();
        map.insert(Value::string("a"), Value::array(values));
        map
    }

    #[test]
    fn array_at_cap_is_accepted() {
        encode_args_map(&array_map(MAX_ENCODE_PAIRS)).expect("1024 pairs must be accepted");
    }

    #[test]
    fn oversized_array_is_rejected_before_the_1025th_push() {
        let err = encode_args_map(&array_map(MAX_ENCODE_PAIRS + 1))
            .expect_err("1025 array items must hit the pair cap");
        let text = err.to_string().to_ascii_lowercase();
        assert!(
            text.contains("1024") || text.contains("pair"),
            "oversized array must not bypass the encode pair cap, got {text}"
        );
    }
}
