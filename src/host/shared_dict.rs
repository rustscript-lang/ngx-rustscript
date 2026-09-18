// OpenResty-oriented shared dictionary hosts.
//
// Optional trailing arguments are explicit `null`. Fixed-shape multi-return
// values use dedicated result structs; RustScript's VM represents struct values
// with its map carrier at the host ABI boundary.

use std::sync::Arc;

use pd_host_function::pd_host_function;
use vm::HostModuleDescriptor;
use vm::resource::HostResource;
use vm::{
    HostNamedStruct, HostResourceType, HostStructField, HostTypeSchema, ResourceRef,
    ResourceTypeKey, Value, Vm, VmError, VmMap,
};

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

impl HostResourceType for SharedDictHandle {
    const KEY: &'static str = "ngx.shared.dict";
    const DESCRIPTION: &'static str = "Shared dictionary zone handle";
}

fn ret<T: IntoHostCallOutcome>(value: T) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn host_err(msg: impl Into<String>) -> VmError {
    VmError::HostError(msg.into())
}

fn struct_outcome<const N: usize>(fields: [(&'static str, Value); N]) -> CallOutcome {
    let mut carrier = VmMap::new();
    for (name, value) in fields {
        carrier.insert(Value::string(name), value);
    }
    Value::Map(Arc::new(carrier)).into_host_call_outcome()
}

fn error_value(error: Option<&'static str>) -> Value {
    error.map_or(Value::Null, Value::string)
}

#[derive(Debug)]
struct SharedGetResult {
    value: Value,
    flags: i64,
    stale: bool,
    error: Option<&'static str>,
}

impl From<shared_dict::GetResult> for SharedGetResult {
    fn from(result: shared_dict::GetResult) -> Self {
        Self {
            value: dict_value_to_vm(result.value),
            flags: i64::from(result.flags),
            stale: result.stale,
            error: result.error,
        }
    }
}

fn optional_string_schema() -> HostTypeSchema {
    HostTypeSchema::Optional(Box::new(HostTypeSchema::String))
}

fn optional_unknown_schema() -> HostTypeSchema {
    HostTypeSchema::Optional(Box::new(HostTypeSchema::Unknown))
}

fn optional_number_schema() -> HostTypeSchema {
    HostTypeSchema::Optional(Box::new(HostTypeSchema::Number))
}

impl HostNamedStruct for SharedGetResult {
    const NAME: &'static str = "SharedGetResult";

    fn host_struct_fields() -> Vec<HostStructField> {
        vec![
            HostStructField::new("value", optional_unknown_schema()),
            HostStructField::new("flags", HostTypeSchema::Int),
            HostStructField::new("stale", HostTypeSchema::Bool),
            HostStructField::new("error", optional_string_schema()),
        ]
    }
}

impl IntoHostCallOutcome for SharedGetResult {
    fn into_host_call_outcome(self) -> CallOutcome {
        struct_outcome([
            ("value", self.value),
            ("flags", Value::Int(self.flags)),
            ("stale", Value::Bool(self.stale)),
            ("error", error_value(self.error)),
        ])
    }
}

#[derive(Debug)]
struct SharedStoreResult {
    ok: bool,
    error: Option<&'static str>,
    forcible: bool,
}

impl From<shared_dict::StoreResult> for SharedStoreResult {
    fn from(result: shared_dict::StoreResult) -> Self {
        Self {
            ok: result.ok,
            error: result.error,
            forcible: result.forcible,
        }
    }
}

impl HostNamedStruct for SharedStoreResult {
    const NAME: &'static str = "SharedStoreResult";

    fn host_struct_fields() -> Vec<HostStructField> {
        vec![
            HostStructField::new("ok", HostTypeSchema::Bool),
            HostStructField::new("error", optional_string_schema()),
            HostStructField::new("forcible", HostTypeSchema::Bool),
        ]
    }
}

impl IntoHostCallOutcome for SharedStoreResult {
    fn into_host_call_outcome(self) -> CallOutcome {
        struct_outcome([
            ("ok", Value::Bool(self.ok)),
            ("error", error_value(self.error)),
            ("forcible", Value::Bool(self.forcible)),
        ])
    }
}

#[derive(Debug)]
struct SharedSafeStoreResult {
    ok: bool,
    error: Option<&'static str>,
}

impl From<shared_dict::StoreResult> for SharedSafeStoreResult {
    fn from(result: shared_dict::StoreResult) -> Self {
        Self {
            ok: result.ok,
            error: result.error,
        }
    }
}

impl HostNamedStruct for SharedSafeStoreResult {
    const NAME: &'static str = "SharedSafeStoreResult";

    fn host_struct_fields() -> Vec<HostStructField> {
        vec![
            HostStructField::new("ok", HostTypeSchema::Bool),
            HostStructField::new("error", optional_string_schema()),
        ]
    }
}

impl IntoHostCallOutcome for SharedSafeStoreResult {
    fn into_host_call_outcome(self) -> CallOutcome {
        struct_outcome([
            ("ok", Value::Bool(self.ok)),
            ("error", error_value(self.error)),
        ])
    }
}

#[derive(Debug)]
struct SharedIncrResult {
    value: Value,
    error: Option<&'static str>,
    forcible: bool,
}

impl From<shared_dict::StoreResult> for SharedIncrResult {
    fn from(result: shared_dict::StoreResult) -> Self {
        Self {
            value: dict_value_to_vm(result.value),
            error: result.error,
            forcible: result.forcible,
        }
    }
}

impl HostNamedStruct for SharedIncrResult {
    const NAME: &'static str = "SharedIncrResult";

    fn host_struct_fields() -> Vec<HostStructField> {
        vec![
            HostStructField::new("value", optional_number_schema()),
            HostStructField::new("error", optional_string_schema()),
            HostStructField::new("forcible", HostTypeSchema::Bool),
        ]
    }
}

impl IntoHostCallOutcome for SharedIncrResult {
    fn into_host_call_outcome(self) -> CallOutcome {
        struct_outcome([
            ("value", self.value),
            ("error", error_value(self.error)),
            ("forcible", Value::Bool(self.forcible)),
        ])
    }
}

#[derive(Debug)]
struct SharedNumberResult {
    value: Value,
    error: Option<&'static str>,
}

impl From<shared_dict::GetResult> for SharedNumberResult {
    fn from(result: shared_dict::GetResult) -> Self {
        Self {
            value: dict_value_to_vm(result.value),
            error: result.error,
        }
    }
}

impl From<shared_dict::StoreResult> for SharedNumberResult {
    fn from(result: shared_dict::StoreResult) -> Self {
        Self {
            value: dict_value_to_vm(result.value),
            error: result.error,
        }
    }
}

impl HostNamedStruct for SharedNumberResult {
    const NAME: &'static str = "SharedNumberResult";

    fn host_struct_fields() -> Vec<HostStructField> {
        vec![
            HostStructField::new("value", optional_number_schema()),
            HostStructField::new("error", optional_string_schema()),
        ]
    }
}

impl IntoHostCallOutcome for SharedNumberResult {
    fn into_host_call_outcome(self) -> CallOutcome {
        struct_outcome([("value", self.value), ("error", error_value(self.error))])
    }
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
///
/// The guest catalog type is `Resource(ngx.shared.dict)`; the constructor still
/// returns the raw guest token because `push_resource` requires `&mut Vm`.
#[pd_host_function(name = "ngx::shared::dict", contract = crate::host::catalog::ngx_shared_dict_contract)]
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
#[pd_host_function(name = "ngx::shared::get", contract = crate::host::catalog::ngx_shared_get_contract)]
fn get(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let result = zone_op(&dict, |name| shared_dict::dict_get(name, key, false))?;
    ret(SharedGetResult::from(result))
}

/// `value, flags, stale = dict:get_stale(key)`.
#[pd_host_function(name = "ngx::shared::get_stale", contract = crate::host::catalog::ngx_shared_get_stale_contract)]
fn get_stale(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let result = zone_op(&dict, |name| shared_dict::dict_get(name, key, true))?;
    ret(SharedGetResult::from(result))
}

fn store_host(
    dict: &SharedDictHandle,
    key: String,
    value: Value,
    exptime: Value,
    flags: Value,
    mode: StoreMode,
) -> VmResult<shared_dict::StoreResult> {
    let key = key_bytes(&key)?;
    let value = vm_to_dict_value(&value)?;
    if value.is_none()
        && matches!(
            mode,
            StoreMode::Add | StoreMode::SafeAdd | StoreMode::Replace
        )
    {
        return Ok(shared_dict::StoreResult {
            ok: false,
            error: Some("attempt to add or replace nil values"),
            forcible: false,
            value: None,
        });
    }
    let exptime = optional_exptime(&exptime, "exptime")?;
    let flags = optional_flags(&flags)?;
    zone_op(dict, |name| {
        shared_dict::dict_store(name, key, value.clone(), exptime, flags, mode)
    })
}

/// `success, err, forcible = dict:set(key, value, exptime?, flags?)`.
#[pd_host_function(name = "ngx::shared::set", contract = crate::host::catalog::ngx_shared_set_contract)]
fn set(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
    exptime: Value,
    flags: Value,
) -> VmResult<CallOutcome> {
    ret(SharedStoreResult::from(store_host(
        &dict,
        key,
        value,
        exptime,
        flags,
        StoreMode::Set,
    )?))
}

/// `success, err = dict:safe_set(...)` without LRU eviction.
#[pd_host_function(name = "ngx::shared::safe_set", contract = crate::host::catalog::ngx_shared_safe_set_contract)]
fn safe_set(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
    exptime: Value,
    flags: Value,
) -> VmResult<CallOutcome> {
    ret(SharedSafeStoreResult::from(store_host(
        &dict,
        key,
        value,
        exptime,
        flags,
        StoreMode::SafeSet,
    )?))
}

/// `success, err, forcible = dict:add(...)` if the key is absent.
#[pd_host_function(name = "ngx::shared::add", contract = crate::host::catalog::ngx_shared_add_contract)]
fn add(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
    exptime: Value,
    flags: Value,
) -> VmResult<CallOutcome> {
    ret(SharedStoreResult::from(store_host(
        &dict,
        key,
        value,
        exptime,
        flags,
        StoreMode::Add,
    )?))
}

/// `success, err = dict:safe_add(...)` without LRU eviction.
#[pd_host_function(name = "ngx::shared::safe_add", contract = crate::host::catalog::ngx_shared_safe_add_contract)]
fn safe_add(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
    exptime: Value,
    flags: Value,
) -> VmResult<CallOutcome> {
    ret(SharedSafeStoreResult::from(store_host(
        &dict,
        key,
        value,
        exptime,
        flags,
        StoreMode::SafeAdd,
    )?))
}

/// `success, err, forcible = dict:replace(...)` if the key exists.
#[pd_host_function(name = "ngx::shared::replace", contract = crate::host::catalog::ngx_shared_replace_contract)]
fn replace(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
    exptime: Value,
    flags: Value,
) -> VmResult<CallOutcome> {
    ret(SharedStoreResult::from(store_host(
        &dict,
        key,
        value,
        exptime,
        flags,
        StoreMode::Replace,
    )?))
}

/// Unconditionally removes `key`.
#[pd_host_function(name = "ngx::shared::delete", contract = crate::host::catalog::ngx_shared_delete_contract)]
fn delete(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    zone_op(&dict, |name| shared_dict::dict_delete(name, key))?;
    ret(())
}

/// `newval, err, forcible = dict:incr(key, value, init?, init_ttl?)`.
#[pd_host_function(name = "ngx::shared::incr", contract = crate::host::catalog::ngx_shared_incr_contract)]
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
    ret(SharedIncrResult::from(result))
}

/// Remaining TTL in seconds, or `not found`.
#[pd_host_function(name = "ngx::shared::ttl", contract = crate::host::catalog::ngx_shared_ttl_contract)]
fn ttl(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let result = zone_op(&dict, |name| shared_dict::dict_ttl(name, key))?;
    let result = match result {
        Ok(ttl) => SharedNumberResult {
            value: if ttl.fract() == 0.0 {
                Value::Int(ttl as i64)
            } else {
                Value::Float(ttl)
            },
            error: None,
        },
        Err(error) => SharedNumberResult {
            value: Value::Null,
            error: Some(error),
        },
    };
    ret(result)
}

/// Updates the remaining TTL for an existing key.
#[pd_host_function(name = "ngx::shared::expire", contract = crate::host::catalog::ngx_shared_expire_contract)]
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
    ret(SharedSafeStoreResult::from(result))
}

/// Zone capacity in bytes.
#[pd_host_function(name = "ngx::shared::capacity", contract = crate::host::catalog::ngx_shared_capacity_contract)]
fn capacity(dict: ResourceRef<'_, SharedDictHandle>) -> VmResult<CallOutcome> {
    let bytes = zone_op(&dict, shared_dict::dict_capacity)?;
    ret(bytes as i64)
}

/// Approximate free space in bytes.
#[pd_host_function(name = "ngx::shared::free_space", contract = crate::host::catalog::ngx_shared_free_space_contract)]
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
            return ret(SharedNumberResult::from(shared_dict::StoreResult {
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
    ret(SharedNumberResult::from(result))
}

/// `len, err = dict:lpush(key, value)` — prepend a list item.
#[pd_host_function(name = "ngx::shared::lpush", contract = crate::host::catalog::ngx_shared_lpush_contract)]
fn lpush(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
) -> VmResult<CallOutcome> {
    list_push_host(&dict, key, value, true)
}

/// `len, err = dict:rpush(key, value)` — append a list item.
#[pd_host_function(name = "ngx::shared::rpush", contract = crate::host::catalog::ngx_shared_rpush_contract)]
fn rpush(
    dict: ResourceRef<'_, SharedDictHandle>,
    key: String,
    value: Value,
) -> VmResult<CallOutcome> {
    list_push_host(&dict, key, value, false)
}

/// `value, err = dict:lpop(key)`.
#[pd_host_function(name = "ngx::shared::lpop", contract = crate::host::catalog::ngx_shared_lpop_contract)]
fn lpop(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let result = zone_op(&dict, |name| shared_dict::dict_lpop(name, key))?;
    ret(SharedGetResult::from(result))
}

/// `value, err = dict:rpop(key)`.
#[pd_host_function(name = "ngx::shared::rpop", contract = crate::host::catalog::ngx_shared_rpop_contract)]
fn rpop(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let result = zone_op(&dict, |name| shared_dict::dict_rpop(name, key))?;
    ret(SharedGetResult::from(result))
}

/// `len, err = dict:llen(key)`. Missing keys are length 0.
#[pd_host_function(name = "ngx::shared::llen", contract = crate::host::catalog::ngx_shared_llen_contract)]
fn llen(dict: ResourceRef<'_, SharedDictHandle>, key: String) -> VmResult<CallOutcome> {
    let key = key_bytes(&key)?;
    let result = zone_op(&dict, |name| shared_dict::dict_llen(name, key))?;
    ret(SharedNumberResult::from(result))
}

/// Mark up to `MAX_FLUSH_BATCH` keys expired (OpenResty sets `expires=1`).
#[pd_host_function(name = "ngx::shared::flush_all", contract = crate::host::catalog::ngx_shared_flush_all_contract)]
fn flush_all(dict: ResourceRef<'_, SharedDictHandle>) -> VmResult<CallOutcome> {
    zone_op(&dict, shared_dict::dict_flush_all)?;
    ret(())
}

/// Delete up to `max` expired keys. `null`/`0` still obey the project batch cap.
#[pd_host_function(name = "ngx::shared::flush_expired", contract = crate::host::catalog::ngx_shared_flush_expired_contract)]
fn flush_expired(
    dict: ResourceRef<'_, SharedDictHandle>,
    max: Value,
) -> VmResult<CallOutcome> {
    let max = optional_max(&max)?;
    let freed = zone_op(&dict, |name| shared_dict::dict_flush_expired(name, max))?;
    ret(freed)
}

/// Enumerate keys oldest-first. `null`/`0` still obey `MAX_GET_KEYS`.
#[pd_host_function(name = "ngx::shared::get_keys", contract = crate::host::catalog::ngx_shared_get_keys_contract)]
fn get_keys(dict: ResourceRef<'_, SharedDictHandle>, max: Value) -> VmResult<CallOutcome> {
    let max = optional_max(&max)?;
    let keys = zone_op(&dict, |name| shared_dict::dict_get_keys(name, max))?;
    let values = keys
        .into_iter()
        .map(|key| Value::string(String::from_utf8_lossy(&key).into_owned()))
        .collect();
    ret(Value::array(values))
}

fn shared_dict_resource() -> vm::HostResourceTypeMeta {
    vm::HostResourceTypeMeta::of::<crate::shared_dict::SharedDictHandle>()
}

const SHARED_DICT_HOST_MODULE_FUNCTIONS: &[fn() -> vm::HostFunctionDescriptor] = &[
        dict_descriptor,
        get_descriptor,
        get_stale_descriptor,
        set_descriptor,
        safe_set_descriptor,
        add_descriptor,
        safe_add_descriptor,
        replace_descriptor,
        delete_descriptor,
        incr_descriptor,
        ttl_descriptor,
        expire_descriptor,
        capacity_descriptor,
        free_space_descriptor,
        lpush_descriptor,
        rpush_descriptor,
        lpop_descriptor,
        rpop_descriptor,
        llen_descriptor,
        flush_all_descriptor,
        flush_expired_descriptor,
        get_keys_descriptor,
];

pub fn shared_dict_host_module() -> HostModuleDescriptor {
    HostModuleDescriptor {
        name: "ngx.shared",
        functions: SHARED_DICT_HOST_MODULE_FUNCTIONS,
        resources: &[shared_dict_resource],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vm::CallReturn;

    fn outcome_map<T: IntoHostCallOutcome>(value: T) -> Arc<VmMap> {
        match value.into_host_call_outcome() {
            CallOutcome::Return(CallReturn::One(Value::Map(map))) => map,
            other => panic!("expected one map carrier, got {other:?}"),
        }
    }

    fn assert_exact_keys(map: &VmMap, expected: &[&str]) {
        assert_eq!(map.len(), expected.len());
        for key in expected {
            assert!(
                map.get(&Value::string(*key)).is_some(),
                "missing result field {key}"
            );
        }
    }

    #[test]
    fn fixed_result_converters_emit_the_documented_exact_shapes() {
        let get = outcome_map(SharedGetResult {
            value: Value::Null,
            flags: 0,
            stale: false,
            error: None,
        });
        assert_exact_keys(&get, &["value", "flags", "stale", "error"]);

        let store = outcome_map(SharedStoreResult {
            ok: true,
            error: None,
            forcible: false,
        });
        assert_exact_keys(&store, &["ok", "error", "forcible"]);

        let safe_store = outcome_map(SharedSafeStoreResult {
            ok: true,
            error: None,
        });
        assert_exact_keys(&safe_store, &["ok", "error"]);

        let incr = outcome_map(SharedIncrResult {
            value: Value::Float(1.0),
            error: None,
            forcible: false,
        });
        assert_exact_keys(&incr, &["value", "error", "forcible"]);

        let number = outcome_map(SharedNumberResult {
            value: Value::Int(1),
            error: None,
        });
        assert_exact_keys(&number, &["value", "error"]);
    }
}
