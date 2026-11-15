//! Real pd-vm tests for L1 ngx codec and hash hosts.
//!
//! OpenResty optional trailing arguments are passed as explicit `null` because
//! the pinned compiler matches host arity exactly.

use ngx_rustscript_nginx_test_stubs as _;

use std::collections::HashSet;
use std::sync::Arc;

use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::bind_ngx_hosts;
use vm::{Program, Value, Vm, VmStatus};

const MAX_CODEC_INPUT_BYTES: usize = 65_536;
const MAX_DECODE_ARGS: usize = 1_024;

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}"))
}

fn eval(source: &str) -> Value {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    let status = vm.run().unwrap_or_else(|err| panic!("run failed: {err}"));
    assert_eq!(status, VmStatus::Halted, "codec hosts are synchronous");
    vm.stack()
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("expected a stack value for {source}"))
}

fn eval_err(source: &str) -> String {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    match vm.run() {
        Err(err) => err.to_string(),
        Ok(status) => panic!("expected host failure, got {status:?}"),
    }
}

fn expect_string(value: Value) -> String {
    match value {
        Value::String(text) => (*text).clone(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn expect_bytes(value: Value) -> Vec<u8> {
    match value {
        Value::Bytes(bytes) => (*bytes).clone(),
        other => panic!("expected bytes, got {other:?}"),
    }
}

fn expect_int(value: Value) -> i64 {
    match value {
        Value::Int(number) => number,
        other => panic!("expected int, got {other:?}"),
    }
}

fn expect_map(value: &Value) -> &vm::VmMap {
    match value {
        Value::Map(map) => map,
        other => panic!("expected map, got {other:?}"),
    }
}

fn map_get<'a>(map: &'a vm::VmMap, key: &str) -> &'a Value {
    map.get(&Value::string(key))
        .unwrap_or_else(|| panic!("missing map key {key}"))
}

/// Unwraps a canonical guest `Option` carrier into its payload.
fn option_payload(value: &Value) -> Option<Value> {
    let (some, payload) = vm::option::payload(value).expect("Option carrier");
    if some {
        Some(payload.expect("Some payload").clone())
    } else {
        None
    }
}

fn expect_option_bytes(value: Value) -> Option<Vec<u8>> {
    option_payload(&value).map(|payload| match payload {
        Value::Bytes(bytes) => bytes.as_ref().clone(),
        other => panic!("expected bytes payload, got {other:?}"),
    })
}

fn expect_option_string_value(value: &Value) -> Option<String> {
    option_payload(value).map(|payload| match payload {
        Value::String(text) => (*text).clone(),
        other => panic!("expected string payload, got {other:?}"),
    })
}

/// `(name, value)` pairs from a typed argument entry array.
fn arg_pairs(root: &Value) -> Vec<(String, Option<String>)> {
    let entries = match map_get(expect_map(root), "args") {
        Value::Array(items) => items,
        other => panic!("expected entry array, got {other:?}"),
    };
    entries
        .iter()
        .map(|entry| {
            let entry = expect_map(entry);
            let name = expect_string(map_get(entry, "name").clone());
            (name, expect_option_string_value(map_get(entry, "value")))
        })
        .collect()
}

fn query_set(encoded: &str) -> HashSet<&str> {
    if encoded.is_empty() {
        HashSet::new()
    } else {
        encoded.split('&').collect()
    }
}

fn decode_root(query: &str, max_args: &str) -> Value {
    eval(&format!(
        "use ngx;\nngx::decode_args(\"{query}\", {max_args});\n"
    ))
}

#[test]
fn escape_uri_component_escapes_space_by_default_type_2() {
    let escaped = expect_string(eval("use ngx;\nngx::escape_uri(\"hello world\", ());\n"));
    assert_eq!(escaped, "hello%20world");
    let typed = expect_string(eval("use ngx;\nngx::escape_uri(\"hello world\", 2);\n"));
    assert_eq!(typed, "hello%20world");
}

#[test]
fn escape_uri_type_0_leaves_unreserved_uri_chars() {
    let escaped = expect_string(eval("use ngx;\nngx::escape_uri(\"a/b-c_d.e~?f\", 0);\n"));
    assert_eq!(escaped, "a/b-c_d.e~%3Ff");
}

#[test]
fn escape_uri_types_1_and_3_through_8_match_openresty_bitmaps() {
    assert_eq!(
        expect_string(eval("use ngx;\nngx::escape_uri(\"a+b c\", 1);\n")),
        "a%2Bb%20c"
    );
    assert_eq!(
        expect_string(eval("use ngx;\nngx::escape_uri(\"a\\\"b;c\", 3);\n")),
        "a%22b;c"
    );
    assert_eq!(
        expect_string(eval("use ngx;\nngx::escape_uri(\"a\\\"b;c\", 4);\n")),
        "a%22b;c"
    );
    assert_eq!(
        expect_string(eval("use ngx;\nngx::escape_uri(\"a+b c\", 5);\n")),
        "a+b%20c"
    );
    assert_eq!(
        expect_string(eval("use ngx;\nngx::escape_uri(\"a+b c\", 6);\n")),
        "a+b%20c"
    );
    assert_eq!(
        expect_string(eval("use ngx;\nngx::escape_uri(\"a\\\"b;c\", 7);\n")),
        "a%22b%3Bc"
    );
    assert_eq!(
        expect_string(eval("use ngx;\nngx::escape_uri(\"a+b c\", 8);\n")),
        "a+b c"
    );
}

#[test]
fn unescape_uri_plus_invalid_percent_and_nul() {
    assert_eq!(
        expect_string(eval("use ngx;\nngx::unescape_uri(\"hello+world\");\n")),
        "hello world"
    );
    assert_eq!(
        expect_string(eval("use ngx;\nngx::unescape_uri(\"%ZZ%2\");\n")),
        "%ZZ%2"
    );
    assert_eq!(
        expect_string(eval("use ngx;\nngx::unescape_uri(\"a%00b\");\n")),
        "a\0b"
    );
    assert_eq!(
        expect_string(eval("use ngx;\nngx::unescape_uri(\"a\\0b\");\n")),
        "a\0b"
    );
}

#[test]
fn unescape_uri_keeps_non_utf8_bytes() {
    let value = eval("use ngx;\nngx::unescape_uri(\"%FF\");\n");
    match value {
        Value::String(text) => {
            assert_eq!(text.as_ref(), "\u{00FF}");
            assert_eq!(text.chars().count(), 1);
        }
        Value::Bytes(bytes) => assert_eq!(&*bytes, &[0xFF]),
        other => panic!("non-utf8 unescape must not fail, got {other:?}"),
    }
}

#[test]
fn encode_args_repeated_empty_and_array_values() {
    let single = expect_string(eval("use ngx;\nngx::encode_args({ \"foo\": 3 });\n"));
    assert_eq!(single, "foo=3");

    let mixed = expect_string(eval(
        "use ngx;\nngx::encode_args({ \"foo\": 3, \"b r\": \"hello world\" });\n",
    ));
    assert_eq!(
        query_set(&mixed),
        ["foo=3", "b%20r=hello%20world"].into_iter().collect()
    );

    let empty_value = expect_string(eval("use ngx;\nngx::encode_args({ \"a\": \"\" });\n"));
    assert_eq!(empty_value, "a=");

    let array_values = expect_string(eval(
        "use ngx;\nngx::encode_args({ \"baz\": [32, \"hello\"] });\n",
    ));
    assert_eq!(array_values, "baz=32&baz=hello");

    let flags = expect_string(eval(
        "use ngx;\nngx::encode_args({ \"on\": true, \"off\": false });\n",
    ));
    assert_eq!(flags, "on");

    // VmMap duplicate keys overwrite; OpenResty multi-value args use arrays.
    let empty = expect_string(eval("use ngx;\nngx::encode_args({});\n"));
    assert_eq!(empty, "");
}

#[test]
fn encode_args_array_true_is_a_bare_key_not_empty_value() {
    let encoded = expect_string(eval(
        "use ngx;\nngx::encode_args({ \"foo\": [32, true, false] });\n",
    ));
    assert_eq!(encoded, "foo=32&foo");
}

#[test]
fn encode_args_enforces_pair_cap_inside_array_expansion() {
    let small = expect_string(eval(
        "use ngx;\nngx::encode_args({ \"a\": [1, 2, 3, 4] });\n",
    ));
    assert_eq!(small, "a=1&a=2&a=3&a=4");
    assert!(small.len() > "a=1".len());

    let flags = expect_string(eval(
        "use ngx;\nngx::encode_args({ \"flag\": [true, true, true] });\n",
    ));
    assert_eq!(flags, "flag&flag&flag");
}

#[test]
fn decode_args_repeated_keys_empty_values_and_truncation() {
    let decoded = eval("use ngx;\nngx::decode_args(\"a=1&b=2&a=3\", ());\n");
    let root = expect_map(&decoded);
    assert_eq!(map_get(root, "truncated"), &Value::Bool(false));
    assert_eq!(
        arg_pairs(&decoded),
        vec![
            ("a".to_string(), Some("1".to_string())),
            ("b".to_string(), Some("2".to_string())),
            ("a".to_string(), Some("3".to_string())),
        ],
        "repeated keys repeat the entry"
    );

    let empty = eval("use ngx;\nngx::decode_args(\"a=&b\", ());\n");
    assert_eq!(
        arg_pairs(&empty),
        vec![
            ("a".to_string(), Some(String::new())),
            ("b".to_string(), None),
        ],
        "an empty value is Some(\"\"); a bare key is None"
    );

    let truncated = eval("use ngx;\nngx::decode_args(\"a=1&b=2&c=3\", 2);\n");
    let truncated_root = expect_map(&truncated);
    assert_eq!(map_get(truncated_root, "truncated"), &Value::Bool(true));
    assert_eq!(arg_pairs(&truncated).len(), 2);
}

#[test]
fn decode_args_flags_empty_keys_nul_and_invalid_percent() {
    let flags = decode_root("foo&bar=1&foo=2", "10");
    assert_eq!(
        arg_pairs(&flags),
        vec![
            ("foo".to_string(), None),
            ("bar".to_string(), Some("1".to_string())),
            ("foo".to_string(), Some("2".to_string())),
        ],
        "a bare key carries Option::None; repeats repeat the entry"
    );

    let empty_keys = decode_root("=hello&=world&ok=1", "10");
    let keys = arg_pairs(&empty_keys);
    assert!(keys.iter().all(|(name, _)| !name.is_empty()));
    assert_eq!(keys, vec![("ok".to_string(), Some("1".to_string()))]);

    let invalid = decode_root("a=%ZZb&b=%00c", "10");
    assert_eq!(
        arg_pairs(&invalid),
        vec![
            ("a".to_string(), Some("%ZZb".to_string())),
            ("b".to_string(), Some("\0c".to_string())),
        ]
    );
}

#[test]
fn decode_args_max_args_zero_means_hard_max_and_1024_is_accepted() {
    let many = (0..MAX_DECODE_ARGS)
        .map(|i| format!("k{i}=1"))
        .collect::<Vec<_>>()
        .join("&");
    let at_cap = decode_root(&many, "0");
    assert_eq!(
        map_get(expect_map(&at_cap), "truncated"),
        &Value::Bool(false)
    );
    assert_eq!(arg_pairs(&at_cap).len(), MAX_DECODE_ARGS);

    let over = format!("{many}&extra=1");
    let truncated = decode_root(&over, "0");
    assert_eq!(
        map_get(expect_map(&truncated), "truncated"),
        &Value::Bool(true)
    );
    assert_eq!(arg_pairs(&truncated).len(), MAX_DECODE_ARGS);

    let explicit_cap = decode_root(&many, "1024");
    assert_eq!(
        map_get(expect_map(&explicit_cap), "truncated"),
        &Value::Bool(false)
    );

    let err = eval_err("use ngx;\nngx::decode_args(\"a=1\", 1025);\n");
    let lower = err.to_ascii_lowercase();
    assert!(
        lower.contains("1024") || lower.contains("max_args") || lower.contains("max args"),
        "max_args above 1024 must be rejected, got {err}"
    );
}

#[test]
fn decode_args_trailing_empty_ampersand_at_max_args_is_not_truncated() {
    let decoded = eval("use ngx;\nngx::decode_args(\"a=1&b=2&\", 2);\n");
    let root = expect_map(&decoded);
    assert_eq!(
        map_get(root, "truncated"),
        &Value::Bool(false),
        "trailing empty & at max_args must not mark truncated"
    );
    assert_eq!(
        arg_pairs(&decoded),
        vec![
            ("a".to_string(), Some("1".to_string())),
            ("b".to_string(), Some("2".to_string())),
        ]
    );
}

#[test]
fn encode_base64_padding_and_decode_variants() {
    assert_eq!(
        expect_string(eval("use ngx;\nngx::encode_base64(\"hello world\", ());\n")),
        "aGVsbG8gd29ybGQ="
    );
    assert_eq!(
        expect_string(eval(
            "use ngx;\nngx::encode_base64(\"hello world\", true);\n"
        )),
        "aGVsbG8gd29ybGQ"
    );
    assert_eq!(
        expect_option_bytes(eval(
            "use ngx;\nngx::decode_base64(\"aGVsbG8gd29ybGQ=\");\n"
        )),
        Some(b"hello world".to_vec())
    );
    assert_eq!(
        expect_option_bytes(eval("use ngx;\nngx::decode_base64(\"aGVsbG8gd29ybGQ\");\n")),
        Some(b"hello world".to_vec())
    );
    assert_eq!(
        expect_option_bytes(eval("use ngx;\nngx::decode_base64(\"!!!\");\n")),
        None,
        "invalid base64 is Option::None"
    );

    let mime = expect_option_bytes(eval(
        "use ngx;\nngx::decode_base64mime(\"aGVs bG8g\\nd29y bGQ=\");\n",
    ));
    assert_eq!(mime, Some(b"hello world".to_vec()));
    assert_eq!(
        expect_option_bytes(eval("use ngx;\nngx::decode_base64mime(\"a\");\n")),
        None
    );
}

#[test]
fn decode_base64mime_skips_garbage_and_stops_at_first_equals() {
    assert_eq!(
        expect_option_bytes(eval(
            "use ngx;\nngx::decode_base64mime(\"!aGVs bG8gd29ybGQ=\");\n"
        )),
        Some(b"hello world".to_vec())
    );
    assert_eq!(
        expect_option_bytes(eval(
            "use ngx;\nngx::decode_base64mime(\"aGVs=bG8gd29ybGQ=\");\n"
        )),
        Some(b"hel".to_vec())
    );
    assert_eq!(
        expect_option_bytes(eval("use ngx;\nngx::decode_base64mime(\"zaaLg==\");\n")),
        None
    );
}

#[test]
fn hash_and_hmac_use_fixed_vectors() {
    assert_eq!(
        expect_string(eval("use ngx;\nngx::md5(\"hello\");\n")),
        "5d41402abc4b2a76b9719d911017c592"
    );
    assert_eq!(
        expect_bytes(eval("use ngx;\nngx::md5_bin(\"hello\");\n")),
        hex::decode_or_panic("5d41402abc4b2a76b9719d911017c592")
    );
    assert_eq!(
        expect_bytes(eval("use ngx;\nngx::sha1_bin(\"hello\");\n")),
        hex::decode_or_panic("aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d")
    );
    assert_eq!(
        expect_int(eval("use ngx;\nngx::crc32_short(\"hello\");\n")),
        0x3610_a686
    );
    assert_eq!(
        expect_int(eval("use ngx;\nngx::crc32_long(\"hello\");\n")),
        0x3610_a686
    );

    let mac = expect_bytes(eval(
        "use ngx;\nngx::hmac_sha1(\"thisisverysecretstuff\", \"some string we want to sign\");\n",
    ));
    let encoded = expect_string(eval(
        "use ngx;\nngx::encode_base64(ngx::hmac_sha1(\"thisisverysecretstuff\", \"some string we want to sign\"), ());\n",
    ));
    assert_eq!(encoded, "R/pvxzHC4NLtj7S+kXFg/NePTmk=");
    assert_eq!(mac.len(), 20);
}

#[test]
fn quote_sql_str_uses_mysql_escapes() {
    assert_eq!(
        expect_string(eval("use ngx;\nngx::quote_sql_str(\"hello\");\n")),
        "'hello'"
    );
    assert_eq!(
        expect_string(eval(
            "use ngx;\nngx::quote_sql_str(\"a'b\\\\c\\\"d\\0e\\nf\");\n"
        )),
        "'a\\'b\\\\c\\\"d\\0e\\nf'"
    );
    assert_eq!(
        expect_string(eval(
            "use ngx;\nngx::quote_sql_str(ngx::unescape_uri(\"%08%09%1A\"));\n"
        )),
        "'\\b\\t\\Z'"
    );
}

#[test]
fn codec_rejects_oversized_input_without_huge_allocation() {
    let oversized = "a".repeat(MAX_CODEC_INPUT_BYTES + 1);
    let err = eval_err(&format!("use ngx;\nngx::escape_uri(\"{oversized}\", 2);\n"));
    let lower = err.to_ascii_lowercase();
    assert!(
        lower.contains("limit") || lower.contains("too long") || lower.contains("max"),
        "oversized input must be a structured host limit failure, got {err}"
    );
}

mod hex {
    pub fn decode_or_panic(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex"))
            .collect()
    }
}
