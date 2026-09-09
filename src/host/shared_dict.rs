// OpenResty-oriented shared dictionary hosts.
//
// Optional trailing arguments are explicit `null`. Multi-return values are
// maps with fixed keys (`value`/`flags`/`stale`/`ok`/`error`/`forcible`).

use std::sync::Arc;

use pd_host_function::pd_host_function;
use vm::resource::HostResource;
use vm::{ResourceRef, ResourceTypeKey, Value, Vm, VmError, VmMap};

use super::{CallOutcome, IntoHostCallOutcome};
use super::super::VmResult;
use crate::shared_dict::{
    self, DictValue, SharedDictHandle, StoreMode, MAX_KEY_BYTES, MAX_VALUE_BYTES,
};

impl HostResource for SharedDictHandle {
    fn resource_type_key() -> Option<ResourceTypeKey>
    where
        Self: Sized,
    {
        Some(shared_dict::shared_dict_resource_key())
    }
}

fn ret<T: IntoHostCallOutcome>(value: T) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn host_err(msg: impl Into<String>) -> VmError {
    VmError::HostError(msg.into())
}

fn insert_map(map: &mut VmMap, key: &str, value: Value) {
    map.insert(Value::string(key), value);
}

fn get_map(result: shared_dict::GetResult) -> Value {
    let mut map = VmMap::new();
    insert_map(&mut map, "value", dict_value_to_vm(result.value));
    insert_map(&mut map, "flags", Value::Int(i64::from(result.flags)));
    insert_map(&mut map, "stale", Value::Bool(result.stale));
    insert_map(
        &mut map,
        "error",
        match result.error {
            Some(err) => Value::String(err.to_string().into()),
            None => Value::Null,
        },
    );
    Value::Map(Arc::new(map))
}

fn store_map(result: shared_dict::StoreResult, include_forcible: bool) -> Value {
    let mut map = VmMap::new();
    insert_map(&mut map, "ok", Value::Bool(result.ok));
    insert_map(
        &mut map,
        "error",
        match result.error {
            Some(err) => Value::String(err.to_string().into()),
            None => Value::Null,
        },
    );
    if include_forcible {
        insert_map(&mut map, "forcible", Value::Bool(result.forcible));
    }
    if let Some(value) = result.value {
        insert_map(&mut map, "value", dict_value_to_vm(Some(value)));
    }
    Value::Map(Arc::new(map))
}

fn incr_map(result: shared_dict::StoreResult) -> Value {
    let mut map = VmMap::new();
    insert_map(&mut map, "value", dict_value_to_vm(result.value));
    insert_map(
        &mut map,
        "error",
        match result.error {
            Some(err) => Value::String(err.to_string().into()),
            None => Value::Null,
        },
    );
    insert_map(&mut map, "forcible", Value::Bool(result.forcible));
    Value::Map(Arc::new(map))
}

fn dict_value_to_vm(value: Option<DictValue>) -> Value {
    match value {
        None => Value::Null,
        Some(DictValue::Bool(flag)) => Value::Bool(flag),
        Some(DictValue::Int(number)) => Value::Int(number),
        Some(DictValue::Float(number)) => Value::Float(number),
        Some(DictValue::String(text)) => Value::String(text.into()),
        Some(DictValue::Bytes(bytes)) => Value::Bytes(bytes.into()),
    }
}

fn vm_to_dict_value(value: &Value) -> VmResult<Option<DictValue>> {
    match value {
        Value::Null => Ok(None),
        Value::Bool(flag) => Ok(Some(DictValue::Bool(*flag))),
        Value::Int(number) => Ok(Some(DictValue::Int(*number))),
        Value::Float(number) => {
            if !number.is_finite() {
                return Err(host_err("shared dict rejects non-finite numbers"));
            }
            Ok(Some(DictValue::Float(*number)))
        }
        Value::String(text) => {
            if text.len() > MAX_VALUE_BYTES {
                return Err(host_err(format!(
                    "shared dict value exceeds the {MAX_VALUE_BYTES}-byte limit"
                )));
            }
            Ok(Some(DictValue::String((**text).clone())))
        }
        Value::Bytes(bytes) => {
            if bytes.len() > MAX_VALUE_BYTES {
                return Err(host_err(format!(
                    "shared dict value exceeds the {MAX_VALUE_BYTES}-byte limit"
                )));
            }
            Ok(Some(DictValue::Bytes((**bytes).clone())))
        }
        _ => Err(host_err("shared dict value must be null, bool, number, string, or bytes")),
    }
}

fn optional_exptime(value: &Value, label: &str) -> VmResult<f64> {
    match value {
        Value::Null => Ok(0.0),
        Value::Int(number) if *number >= 0 => Ok(*number as f64),
        Value::Float(number) if number.is_finite() && *number >= 0.0 => Ok(*number),
        Value::Int(_) | Value::Float(_) => Err(host_err(format!("{label} must be >= 0"))),
        _ => Err(host_err(format!("{label} expects number or null"))),
    }
}

fn optional_flags(value: &Value) -> VmResult<u32> {
    match value {
        Value::Null => Ok(0),
        Value::Int(number) if *number >= 0 && *number <= i64::from(u32::MAX) => {
            Ok(*number as u32)
        }
        _ => Err(host_err("flags expects a uint32 or null")),
    }
}

fn optional_number(value: &Value, label: &str) -> VmResult<Option<DictValue>> {
    match value {
        Value::Null => Ok(None),
        Value::Int(number) => Ok(Some(DictValue::Int(*number))),
        Value::Float(number) if number.is_finite() => Ok(Some(DictValue::Float(*number))),
        Value::Float(_) => Err(host_err(format!("{label} rejects non-finite numbers"))),
        _ => Err(host_err(format!("{label} expects number or null"))),
    }
}

fn key_bytes(key: &str) -> VmResult<&[u8]> {
    if key.len() > MAX_KEY_BYTES {
        return Err(host_err(format!(
            "shared dict key exceeds the {MAX_KEY_BYTES}-byte limit"
        )));
    }
    Ok(key.as_bytes())
}

fn zone_op<T>(
    dict: &SharedDictHandle,
    op: impl FnOnce(&str) -> Result<T, &'static str>,
) -> VmResult<T> {
    shared_dict::require_handle(dict).map_err(|err| {
        if err == "not found" {
            host_err(format!("shared dict \"{}\" not found", dict.name))
        } else {
            host_err(err)
        }
    })?;
    op(&dict.name).map_err(|err| {
        if err == "not found" {
            host_err(format!("shared dict \"{}\" not found", dict.name))
        } else {
            host_err(err)
        }
    })
}

/// Fetch a typed handle for the named `rss_shared_dict` zone.
#[pd_host_function(name = "ngx::shared::dict")]
fn dict(vm: &mut Vm, name: String) -> VmResult<CallOutcome> {
    let handle = shared_dict::open_handle(&name).map_err(|err| {
        if err == "not found" {
            host_err(format!("shared dict \"{name}\" not found"))
        } else {
            host_err(err)
        }
    })?;
    let handle = vm
        .host_context()
        .push_resource(handle)
        .map_err(|err| host_err(err.to_string()))?;
    ret(handle.handle().raw() as i64)
}

/// `value, flags = dict:get(key)` as `{ value, flags, stale, error }`.
#[pd_host_function(name = "ngx::shared::get")]
fn get(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let result = zone_op(&dict, |name| shared_dict::dict_get(name, key, false))?;
    ret(get_map(result))
}

/// `value, flags, stale = dict:get_stale(key)`.
#[pd_host_function(name = "ngx::shared::get_stale")]
fn get_stale(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let result = zone_op(&dict, |name| shared_dict::dict_get(name, key, true))?;
    ret(get_map(result))
}

fn store_host(
    dict: &SharedDictHandle,
    key: String,
    value: Value,
    exptime: Value,
    flags: Value,
    mode: StoreMode,
    include_forcible: bool,
) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let value = vm_to_dict_value(&value)?;
    if value.is_none()
        && matches!(
            mode,
            StoreMode::Add | StoreMode::SafeAdd | StoreMode::Replace
        )
    {
        return ret(store_map(
            shared_dict::StoreResult {
                ok: false,
                error: Some("attempt to add or replace nil values"),
                forcible: false,
                value: None,
            },
            include_forcible,
        ));
    }
    let exptime = optional_exptime(&exptime, "exptime")?;
    let flags = optional_flags(&flags)?;
    let result = zone_op(dict, |name| {
        shared_dict::dict_store(name, key, value.clone(), exptime, flags, mode)
    })?;
    ret(store_map(result, include_forcible))
}

/// `success, err, forcible = dict:set(key, value, exptime?, flags?)`.
#[pd_host_function(name = "ngx::shared::set")]
fn set(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
    exptime: Value,
    flags: Value,
) -> VmResult<CallOutcome> {
    store_host(&dict, key, value, exptime, flags, StoreMode::Set, true)
}

/// `success, err = dict:safe_set(...)` without LRU eviction.
#[pd_host_function(name = "ngx::shared::safe_set")]
fn safe_set(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
    exptime: Value,
    flags: Value,
) -> VmResult<CallOutcome> {
    store_host(&dict, key, value, exptime, flags, StoreMode::SafeSet, false)
}

/// `success, err, forcible = dict:add(...)` if the key is absent.
#[pd_host_function(name = "ngx::shared::add")]
fn add(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
    exptime: Value,
    flags: Value,
) -> VmResult<CallOutcome> {
    store_host(&dict, key, value, exptime, flags, StoreMode::Add, true)
}

/// `success, err = dict:safe_add(...)` without LRU eviction.
#[pd_host_function(name = "ngx::shared::safe_add")]
fn safe_add(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
    exptime: Value,
    flags: Value,
) -> VmResult<CallOutcome> {
    store_host(&dict, key, value, exptime, flags, StoreMode::SafeAdd, false)
}

/// `success, err, forcible = dict:replace(...)` if the key exists.
#[pd_host_function(name = "ngx::shared::replace")]
fn replace(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
    exptime: Value,
    flags: Value,
) -> VmResult<CallOutcome> {
    store_host(&dict, key, value, exptime, flags, StoreMode::Replace, true)
}

/// Unconditionally removes `key`.
#[pd_host_function(name = "ngx::shared::delete")]
fn delete(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    zone_op(&dict, |name| shared_dict::dict_delete(name, key))?;
    ret(())
}

/// `newval, err, forcible = dict:incr(key, value, init?, init_ttl?)`.
#[pd_host_function(name = "ngx::shared::incr")]
fn incr(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
    init: Value,
    init_ttl: Value,
) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let delta = optional_number(&value, "incr value")?
        .ok_or_else(|| host_err("incr value expects a number"))?;
    let init = optional_number(&init, "init")?;
    let init_ttl = if matches!(init_ttl, Value::Null) {
        0.0
    } else {
        if init.is_none() {
            return Err(host_err("init_ttl requires init"));
        }
        optional_exptime(&init_ttl, "init_ttl")?
    };
    let result = zone_op(&dict, |name| {
        shared_dict::dict_incr(name, key, delta.clone(), init.clone(), init_ttl)
    })?;
    ret(incr_map(result))
}

/// Remaining TTL in seconds, or `not found`.
#[pd_host_function(name = "ngx::shared::ttl")]
fn ttl(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let result = zone_op(&dict, |name| shared_dict::dict_ttl(name, key))?;
    let mut map = VmMap::new();
    match result {
        Ok(ttl) => {
            insert_map(
                &mut map,
                "value",
                if ttl.fract() == 0.0 {
                    Value::Int(ttl as i64)
                } else {
                    Value::Float(ttl)
                },
            );
            insert_map(&mut map, "error", Value::Null);
        }
        Err(err) => {
            insert_map(&mut map, "value", Value::Null);
            insert_map(&mut map, "error", Value::String(err.to_string().into()));
        }
    }
    ret(Value::Map(Arc::new(map)))
}

/// Updates the remaining TTL for an existing key.
#[pd_host_function(name = "ngx::shared::expire")]
fn expire(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    exptime: Value,
) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let exptime = optional_exptime(&exptime, "exptime")?;
    let result = zone_op(&dict, |name| {
        shared_dict::dict_expire(name, key, exptime)
    })?;
    ret(store_map(result, false))
}

/// Zone capacity in bytes.
#[pd_host_function(name = "ngx::shared::capacity")]
fn capacity(dict: ResourceRef<'_, SharedDictHandle>) -> VmResult<CallOutcome> {
    let bytes = zone_op(&dict, shared_dict::dict_capacity)?;
    ret(bytes as i64)
}

/// Approximate free space in bytes.
#[pd_host_function(name = "ngx::shared::free_space")]
fn free_space(dict: ResourceRef<'_, SharedDictHandle>) -> VmResult<CallOutcome> {
    let bytes = zone_op(&dict, shared_dict::dict_free_space)?;
    ret(bytes as i64)
}

fn list_item_from_vm(value: &Value) -> Result<DictValue, &'static str> {
    match value {
        Value::Int(number) => Ok(DictValue::Int(*number)),
        Value::Float(number) if number.is_finite() => Ok(DictValue::Float(*number)),
        Value::String(text) if text.len() <= MAX_VALUE_BYTES => {
            Ok(DictValue::String((**text).clone()))
        }
        Value::Bytes(bytes) if bytes.len() <= MAX_VALUE_BYTES => {
            Ok(DictValue::Bytes((**bytes).clone()))
        }
        Value::String(_) | Value::Bytes(_) => Err("value too long"),
        _ => Err("bad value type"),
    }
}

fn list_result_map(result: shared_dict::StoreResult) -> Value {
    let mut map = VmMap::new();
    insert_map(&mut map, "value", dict_value_to_vm(result.value));
    insert_map(
        &mut map,
        "error",
        match result.error {
            Some(err) => Value::String(err.to_string().into()),
            None => Value::Null,
        },
    );
    Value::Map(Arc::new(map))
}

fn optional_max(value: &Value) -> VmResult<Option<i64>> {
    match value {
        Value::Null => Ok(None),
        Value::Int(number) => Ok(Some(*number)),
        Value::Float(number) if number.is_finite() && number.fract() == 0.0 => {
            Ok(Some(*number as i64))
        }
        _ => Err(host_err("max expects an integer or null")),
    }
}

fn list_push_host(
    dict: &SharedDictHandle,
    key: String,
    value: Value,
    left: bool,
) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let item = match list_item_from_vm(&value) {
        Ok(item) => item,
        Err(err) => {
            return ret(list_result_map(shared_dict::StoreResult {
                ok: false,
                error: Some(err),
                forcible: false,
                value: None,
            }));
        }
    };
    let result = zone_op(dict, |name| {
        if left {
            shared_dict::dict_lpush(name, key, item.clone())
        } else {
            shared_dict::dict_rpush(name, key, item.clone())
        }
    })?;
    ret(list_result_map(result))
}

/// `len, err = dict:lpush(key, value)` — prepend a list item.
#[pd_host_function(name = "ngx::shared::lpush")]
fn lpush(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
) -> VmResult<CallOutcome> {
    list_push_host(&dict, key, value, true)
}

/// `len, err = dict:rpush(key, value)` — append a list item.
#[pd_host_function(name = "ngx::shared::rpush")]
fn rpush(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
) -> VmResult<CallOutcome> {
    list_push_host(&dict, key, value, false)
}

/// `value, err = dict:lpop(key)`.
#[pd_host_function(name = "ngx::shared::lpop")]
fn lpop(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let result = zone_op(&dict, |name| shared_dict::dict_lpop(name, key))?;
    ret(get_map(result))
}

/// `value, err = dict:rpop(key)`.
#[pd_host_function(name = "ngx::shared::rpop")]
fn rpop(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let result = zone_op(&dict, |name| shared_dict::dict_rpop(name, key))?;
    ret(get_map(result))
}

/// `len, err = dict:llen(key)`. Missing keys are length 0.
#[pd_host_function(name = "ngx::shared::llen")]
fn llen(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let result = zone_op(&dict, |name| shared_dict::dict_llen(name, key))?;
    ret(get_map(result))
}

/// Mark up to `MAX_FLUSH_BATCH` keys expired (OpenResty sets `expires=1`).
#[pd_host_function(name = "ngx::shared::flush_all")]
fn flush_all(dict: ResourceRef<'_, SharedDictHandle>) -> VmResult<CallOutcome> {
    zone_op(&dict, shared_dict::dict_flush_all)?;
    ret(())
}

/// Delete up to `max` expired keys. `null`/`0` still obey the project batch cap.
#[pd_host_function(name = "ngx::shared::flush_expired")]
fn flush_expired(
    dict: ResourceRef<'_, SharedDictHandle>,
    max: Value,
) -> VmResult<CallOutcome> {
    let max = optional_max(&max)?;
    let freed = zone_op(&dict, |name| shared_dict::dict_flush_expired(name, max))?;
    ret(freed)
}

/// Enumerate keys oldest-first. `null`/`0` still obey `MAX_GET_KEYS`.
#[pd_host_function(name = "ngx::shared::get_keys")]
fn get_keys(dict: ResourceRef<'_, SharedDictHandle>, max: Value) -> VmResult<CallOutcome> {
    let max = optional_max(&max)?;
    let keys = zone_op(&dict, |name| shared_dict::dict_get_keys(name, max))?;
    let values = keys
        .into_iter()
        .map(|key| Value::string(String::from_utf8_lossy(&key).into_owned()))
        .collect();
    ret(Value::array(values))
}
