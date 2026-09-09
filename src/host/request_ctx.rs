// Request-local ngx.ctx. Owned by AccessExec; hosts clone values out.
//
// Limits (also documented in docs/ngx-api-compatibility.md):
// - MAX_CTX_KEYS 256
// - MAX_CTX_KEY_BYTES 256
// - MAX_CTX_DEPTH 16
// - MAX_CTX_TOTAL_BYTES 65536 (keys + recursive values)
// - MAX_CTX_CONTAINER_ELEMS 1024 per array/map
// - MAX_CTX_SCALAR_BYTES 8192 per string/bytes
// Empty and NUL keys are rejected. delete returns the removed value or null.
// Stored values are deep-owned scalars/containers only (no callables).

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use pd_host_function::pd_host_function;
use vm::{Value, VmError, VmMap};

use super::super::VmResult;
use super::{CallOutcome, IntoHostCallOutcome};
use crate::host::{current_identity, with_current_ctx, with_current_ctx_mut, ScopeError};

/// Maximum number of top-level ctx keys.
pub const MAX_CTX_KEYS: usize = 256;
/// Maximum bytes in a top-level ctx key.
pub const MAX_CTX_KEY_BYTES: usize = 256;
/// Maximum nesting depth of arrays/maps (root value is depth 1).
pub const MAX_CTX_DEPTH: usize = 16;
/// Maximum recursively accounted ctx bytes including keys.
pub const MAX_CTX_TOTAL_BYTES: usize = 65_536;
/// Maximum elements in one array or map.
pub const MAX_CTX_CONTAINER_ELEMS: usize = 1_024;
/// Maximum bytes in one string or bytes scalar.
pub const MAX_CTX_SCALAR_BYTES: usize = 8_192;

const NODE_OVERHEAD: usize = 8;

#[cfg(test)]
thread_local! {
    static LIVE_OWNED_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub fn live_owned_entry_count() -> usize {
    LIVE_OWNED_ENTRIES.with(|cell| cell.get())
}

struct StoredValue {
    value: Value,
    size: usize,
}

/// Request-owned ngx.ctx map. Drop releases every stored value.
pub struct RequestLocalCtx {
    map: BTreeMap<String, StoredValue>,
    bytes: usize,
}

impl Default for RequestLocalCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestLocalCtx {
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            bytes: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn byte_size(&self) -> usize {
        self.bytes
    }

    pub fn get(&self, key: &str) -> Value {
        match self.map.get(key) {
            Some(stored) => clone_owned(&stored.value),
            None => Value::Null,
        }
    }

    pub fn set(&mut self, key: &str, value: &Value) -> Result<(), String> {
        validate_key(key)?;
        let (owned, value_size) = deep_own(value, 1, &mut HashSet::new())?;
        let key_cost = NODE_OVERHEAD
            .checked_add(key.len())
            .ok_or_else(|| "ngx::ctx size overflow".to_string())?;
        let old = self.map.get(key);
        let replacing = old.is_some();
        let old_total = old.map(|stored| stored.size).unwrap_or(0);
        if !replacing && self.map.len() >= MAX_CTX_KEYS {
            return Err(format!("ngx::ctx key count exceeds {MAX_CTX_KEYS}"));
        }
        let without_old = self
            .bytes
            .checked_sub(old_total)
            .ok_or_else(|| "ngx::ctx size overflow".to_string())?;
        let added = if replacing {
            value_size
        } else {
            key_cost
                .checked_add(value_size)
                .ok_or_else(|| "ngx::ctx size overflow".to_string())?
        };
        let new_total = without_old
            .checked_add(added)
            .ok_or_else(|| "ngx::ctx size overflow".to_string())?;
        if new_total > MAX_CTX_TOTAL_BYTES {
            return Err(format!(
                "ngx::ctx total size exceeds {MAX_CTX_TOTAL_BYTES} bytes"
            ));
        }
        let previous = self.map.insert(
            key.to_string(),
            StoredValue {
                value: owned,
                size: value_size,
            },
        );
        self.bytes = new_total;
        match previous {
            Some(_) => {}
            None => account_insert(),
        }
        Ok(())
    }

    pub fn delete(&mut self, key: &str) -> Result<Value, String> {
        validate_key(key)?;
        match self.map.remove(key) {
            Some(stored) => {
                let key_cost = NODE_OVERHEAD
                    .checked_add(key.len())
                    .ok_or_else(|| "ngx::ctx size overflow".to_string())?;
                let released = key_cost
                    .checked_add(stored.size)
                    .ok_or_else(|| "ngx::ctx size overflow".to_string())?;
                self.bytes = self.bytes.saturating_sub(released);
                account_remove();
                Ok(clone_owned(&stored.value))
            }
            None => Ok(Value::Null),
        }
    }

    pub fn clear(&mut self) {
        let n = self.map.len();
        self.map.clear();
        self.bytes = 0;
        account_remove_n(n);
    }
}

impl Drop for RequestLocalCtx {
    fn drop(&mut self) {
        self.clear();
    }
}

fn account_insert() {
    #[cfg(test)]
    LIVE_OWNED_ENTRIES.with(|cell| cell.set(cell.get().saturating_add(1)));
}

fn account_remove() {
    account_remove_n(1);
}

fn account_remove_n(n: usize) {
    #[cfg(test)]
    LIVE_OWNED_ENTRIES.with(|cell| cell.set(cell.get().saturating_sub(n)));
    #[cfg(not(test))]
    let _ = n;
}

fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("ngx::ctx key must not be empty".into());
    }
    if key.as_bytes().contains(&0) {
        return Err("ngx::ctx key must not contain embedded NUL".into());
    }
    if key.len() > MAX_CTX_KEY_BYTES {
        return Err(format!("ngx::ctx key exceeds {MAX_CTX_KEY_BYTES} bytes"));
    }
    Ok(())
}

fn clone_owned(value: &Value) -> Value {
    match value {
        Value::Null | Value::Int(_) | Value::Float(_) | Value::Bool(_) => value.clone(),
        Value::String(text) => Value::string(text.as_str().to_owned()),
        Value::Bytes(bytes) => Value::bytes(bytes.as_ref().to_vec()),
        Value::Array(items) => Value::array(items.iter().map(clone_owned).collect()),
        Value::Map(map) => {
            let mut owned = VmMap::new();
            for (key, item) in map.iter() {
                owned.insert(clone_owned(key), clone_owned(item));
            }
            Value::Map(Arc::new(owned))
        }
        Value::Callable(_) => Value::Null,
    }
}

fn deep_own(
    value: &Value,
    depth: usize,
    seen: &mut HashSet<usize>,
) -> Result<(Value, usize), String> {
    if depth > MAX_CTX_DEPTH {
        return Err(format!("ngx::ctx nesting depth exceeds {MAX_CTX_DEPTH}"));
    }
    match value {
        Value::Callable(_) => Err("ngx::ctx cannot store callable values".into()),
        Value::Null => Ok((Value::Null, NODE_OVERHEAD)),
        Value::Bool(_) => Ok((value.clone(), NODE_OVERHEAD)),
        Value::Int(_) => Ok((value.clone(), NODE_OVERHEAD)),
        Value::Float(_) => Ok((value.clone(), NODE_OVERHEAD)),
        Value::String(text) => {
            if text.len() > MAX_CTX_SCALAR_BYTES {
                return Err(format!(
                    "ngx::ctx scalar exceeds {MAX_CTX_SCALAR_BYTES} bytes"
                ));
            }
            let size = NODE_OVERHEAD
                .checked_add(text.len())
                .ok_or_else(|| "ngx::ctx size overflow".to_string())?;
            Ok((Value::string(text.as_str().to_owned()), size))
        }
        Value::Bytes(bytes) => {
            if bytes.len() > MAX_CTX_SCALAR_BYTES {
                return Err(format!(
                    "ngx::ctx scalar exceeds {MAX_CTX_SCALAR_BYTES} bytes"
                ));
            }
            let size = NODE_OVERHEAD
                .checked_add(bytes.len())
                .ok_or_else(|| "ngx::ctx size overflow".to_string())?;
            Ok((Value::bytes(bytes.as_ref().to_vec()), size))
        }
        Value::Array(items) => {
            let ptr = Arc::as_ptr(items) as usize;
            if !seen.insert(ptr) {
                return Err("ngx::ctx value is cyclic".into());
            }
            if items.len() > MAX_CTX_CONTAINER_ELEMS {
                seen.remove(&ptr);
                return Err(format!(
                    "ngx::ctx container exceeds {MAX_CTX_CONTAINER_ELEMS} elements"
                ));
            }
            let mut owned = Vec::with_capacity(items.len());
            let mut size = NODE_OVERHEAD;
            for item in items.iter() {
                let (child, child_size) = match deep_own(item, depth + 1, seen) {
                    Ok(result) => result,
                    Err(err) => {
                        seen.remove(&ptr);
                        return Err(err);
                    }
                };
                size = match size.checked_add(child_size) {
                    Some(total) => total,
                    None => {
                        seen.remove(&ptr);
                        return Err("ngx::ctx size overflow".into());
                    }
                };
                owned.push(child);
            }
            seen.remove(&ptr);
            Ok((Value::array(owned), size))
        }
        Value::Map(map) => {
            let ptr = Arc::as_ptr(map) as usize;
            if !seen.insert(ptr) {
                return Err("ngx::ctx value is cyclic".into());
            }
            if map.len() > MAX_CTX_CONTAINER_ELEMS {
                seen.remove(&ptr);
                return Err(format!(
                    "ngx::ctx container exceeds {MAX_CTX_CONTAINER_ELEMS} elements"
                ));
            }
            let mut owned = VmMap::new();
            let mut size = NODE_OVERHEAD;
            for (key, item) in map.iter() {
                let (owned_key, key_size) = match deep_own(key, depth + 1, seen) {
                    Ok(result) => result,
                    Err(err) => {
                        seen.remove(&ptr);
                        return Err(err);
                    }
                };
                let (owned_item, item_size) = match deep_own(item, depth + 1, seen) {
                    Ok(result) => result,
                    Err(err) => {
                        seen.remove(&ptr);
                        return Err(err);
                    }
                };
                let entry = match key_size.checked_add(item_size) {
                    Some(total) => total,
                    None => {
                        seen.remove(&ptr);
                        return Err("ngx::ctx size overflow".into());
                    }
                };
                size = match size.checked_add(entry) {
                    Some(total) => total,
                    None => {
                        seen.remove(&ptr);
                        return Err("ngx::ctx size overflow".into());
                    }
                };
                owned.insert(owned_key, owned_item);
            }
            seen.remove(&ptr);
            Ok((Value::Map(Arc::new(owned)), size))
        }
    }
}

fn scope_err(err: ScopeError) -> VmError {
    VmError::HostError(match err {
        ScopeError::NotInRequestScope => {
            "ngx ctx host called outside request execution scope".into()
        }
        other => other.to_string(),
    })
}

fn host_err(message: String) -> VmError {
    VmError::HostError(message)
}

fn ret(value: impl IntoHostCallOutcome) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

/// OpenResty-adapted `ngx.ctx[key]` read. Missing keys return `null`.
#[pd_host_function(name = "ngx::ctx::get")]
pub fn get(key: &str) -> VmResult<CallOutcome> {
    let identity = current_identity().map_err(scope_err)?;
    let value = with_current_ctx(identity, |ctx| ctx.get(key)).map_err(scope_err)?;
    ret(value)
}

/// OpenResty-adapted `ngx.ctx[key] = value`. Returns `null`.
#[pd_host_function(name = "ngx::ctx::set")]
pub fn set(key: &str, value: &Value) -> VmResult<CallOutcome> {
    let identity = current_identity().map_err(scope_err)?;
    with_current_ctx_mut(identity, |ctx| ctx.set(key, value))
        .map_err(scope_err)?
        .map_err(host_err)?;
    ret(())
}

/// Adapted delete: returns the removed value, or `null` if missing.
#[pd_host_function(name = "ngx::ctx::delete")]
pub fn delete(key: &str) -> VmResult<CallOutcome> {
    let identity = current_identity().map_err(scope_err)?;
    let value = with_current_ctx_mut(identity, |ctx| ctx.delete(key))
        .map_err(scope_err)?
        .map_err(host_err)?;
    ret(value)
}

/// Clear every ctx entry. Returns `null`.
#[pd_host_function(name = "ngx::ctx::clear")]
pub fn clear() -> VmResult<CallOutcome> {
    let identity = current_identity().map_err(scope_err)?;
    with_current_ctx_mut(identity, |ctx| ctx.clear()).map_err(scope_err)?;
    ret(())
}

#[cfg(test)]
mod ctx_store_tests {
    use super::*;

    #[test]
    fn drop_releases_owned_entries_exactly_once() {
        let before = live_owned_entry_count();
        {
            let mut ctx = RequestLocalCtx::new();
            ctx.set("a", &Value::Int(1)).unwrap();
            ctx.set("b", &Value::string("x")).unwrap();
            assert_eq!(live_owned_entry_count(), before + 2);
            ctx.clear();
            assert_eq!(live_owned_entry_count(), before);
            ctx.set("c", &Value::Bool(true)).unwrap();
            assert_eq!(live_owned_entry_count(), before + 1);
        }
        assert_eq!(live_owned_entry_count(), before);
    }

    #[test]
    fn failed_replace_is_atomic() {
        let mut ctx = RequestLocalCtx::new();
        ctx.set("k", &Value::string("old")).unwrap();
        let too_big = Value::string("x".repeat(MAX_CTX_SCALAR_BYTES + 1));
        let err = ctx.set("k", &too_big).expect_err("oversize replace");
        assert!(err.contains("scalar"), "{err}");
        match ctx.get("k") {
            Value::String(text) => assert_eq!(text.as_str(), "old"),
            other => panic!("previous value must remain, got {other:?}"),
        }
    }

    #[test]
    fn shared_aliases_do_not_undercount() {
        let chunk = Value::string("n".repeat(MAX_CTX_SCALAR_BYTES));
        let payload = Value::array(vec![chunk; 5]);
        let mut map = VmMap::new();
        map.insert(Value::string("x"), payload.clone());
        map.insert(Value::string("y"), payload);
        let value = Value::Map(Arc::new(map));
        let mut ctx = RequestLocalCtx::new();
        let err = ctx
            .set("a", &value)
            .expect_err("two 5x8KiB aliases must exceed 64KiB");
        assert!(err.contains("total size"), "{err}");
        assert!(ctx.is_empty());
    }

    #[test]
    fn rejects_empty_nul_callable_and_budgets() {
        let mut ctx = RequestLocalCtx::new();
        assert!(ctx.set("", &Value::Int(1)).unwrap_err().contains("empty"));
        assert!(ctx
            .set("a\0b", &Value::Int(1))
            .unwrap_err()
            .contains("NUL"));
        assert!(ctx
            .set(&"k".repeat(MAX_CTX_KEY_BYTES + 1), &Value::Int(1))
            .unwrap_err()
            .contains("key exceeds"));

        let mut nested = Value::Int(1);
        for _ in 0..MAX_CTX_DEPTH {
            nested = Value::array(vec![nested]);
        }
        let err = ctx.set("deep", &nested).expect_err("depth");
        assert!(err.contains("depth"), "{err}");

        let many = Value::array(vec![Value::Int(1); MAX_CTX_CONTAINER_ELEMS + 1]);
        let err = ctx.set("arr", &many).expect_err("elems");
        assert!(err.contains("container"), "{err}");

        for i in 0..MAX_CTX_KEYS {
            ctx.set(&format!("k{i}"), &Value::Int(i as i64)).unwrap();
        }
        let err = ctx.set("overflow", &Value::Int(0)).expect_err("keys");
        assert!(err.contains("key count"), "{err}");
    }
}
