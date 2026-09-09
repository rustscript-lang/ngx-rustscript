//! Shared dictionary core: test-only process-local store plus NGINX shm backend.
//!
//! Hosts live in [`crate::host`]. Production workers use the shm/slab/rbtree
//! path exclusively. The HashMap backend is compiled only under `rustc-test`.

use vm::ResourceTypeKey;

#[path = "shared_dict_index.rs"]
mod index;

pub use index::{DictIndex, nginx_memn2cmp};

pub(crate) use index::ngx_crc32_short;

/// Catalog / runtime resource type key for a shared-dict zone handle.
pub fn shared_dict_resource_key() -> ResourceTypeKey {
    ResourceTypeKey::new("ngx.shared.dict").expect("ngx.shared.dict is a valid resource key")
}

/// Maximum zone name length in bytes (documented ceiling).
pub const MAX_ZONE_NAME_BYTES: usize = 256;
/// OpenResty `lua_shared_dict` key ceiling.
pub const MAX_KEY_BYTES: usize = 65_535;
/// Project value ceiling (aligned with other ngx hosts).
pub const MAX_VALUE_BYTES: usize = 65_536;
/// Project list-length ceiling (OpenResty is unbounded `int`).
pub const MAX_LIST_LEN: usize = 65_536;
/// Hard cap for `get_keys` results *and* LRU visits (OpenResty `get_keys(0)` is unlimited).
pub const MAX_GET_KEYS: usize = 1_024;
/// Per-call cap for `flush_all` / `flush_expired` results *and* LRU visits (OpenResty `max=0` is unlimited).
pub const MAX_FLUSH_BATCH: usize = 1_024;
/// OpenResty hard minimum zone size.
pub const MIN_ZONE_BYTES: usize = 8 * 1024;
/// Approximate per-entry overhead counted against process-local capacity.
#[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
const NODE_OVERHEAD: usize = 64;
/// OpenResty retries roughly tens of LRU victims before `no memory`.
pub(crate) const MAX_EVICT: usize = 30;
/// `long` millisecond TTL ceiling used by OpenResty `exptime * 1000`.
const MAX_EXPTIME_MS: u64 = i64::MAX as u64;

/// Typed unforgeable handle. Lookup is by immutable zone identity + generation.
#[derive(Debug, Clone)]
pub struct SharedDictHandle {
    pub name: String,
    generation: u64,
}

impl SharedDictHandle {
    pub fn zone_name(&self) -> &str {
        &self.name
    }
}

/// Scalar values stored in a shared dict. List keys use a separate node kind.
#[derive(Clone, Debug, PartialEq)]
pub enum DictValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
}

impl DictValue {
    pub(crate) fn stored_len(&self) -> usize {
        match self {
            Self::Bool(_) => 1,
            Self::Int(_) | Self::Float(_) => 8,
            Self::String(text) => text.len(),
            Self::Bytes(bytes) => bytes.len(),
        }
    }

    pub(crate) fn as_number(&self) -> Option<DictValue> {
        match self {
            Self::Int(_) | Self::Float(_) => Some(self.clone()),
            _ => None,
        }
    }
}

/// How a store operation treats an existing key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreMode {
    Set,
    Add,
    Replace,
    SafeSet,
    SafeAdd,
}

/// Result of get / get_stale.
#[derive(Clone, Debug, PartialEq)]
pub struct GetResult {
    pub value: Option<DictValue>,
    pub flags: u32,
    pub stale: bool,
    pub error: Option<&'static str>,
}

/// Result of set / add / replace / safe_* / incr / expire.
#[derive(Clone, Debug, PartialEq)]
pub struct StoreResult {
    pub ok: bool,
    pub error: Option<&'static str>,
    pub forcible: bool,
    pub value: Option<DictValue>,
}

pub(crate) fn empty_get(error: &'static str) -> GetResult {
    GetResult {
        value: None,
        flags: 0,
        stale: false,
        error: Some(error),
    }
}

pub(crate) fn store_err(error: &'static str) -> StoreResult {
    StoreResult {
        ok: false,
        error: Some(error),
        forcible: false,
        value: None,
    }
}

pub(crate) fn store_ok(forcible: bool) -> StoreResult {
    StoreResult {
        ok: true,
        error: None,
        forcible,
        value: None,
    }
}

pub(crate) fn add_numbers(left: &DictValue, right: &DictValue) -> DictValue {
    match (left, right) {
        (DictValue::Int(a), DictValue::Int(b)) => match a.checked_add(*b) {
            Some(sum) => DictValue::Int(sum),
            None => DictValue::Float(*a as f64 + *b as f64),
        },
        (DictValue::Int(a), DictValue::Float(b)) => DictValue::Float(*a as f64 + *b),
        (DictValue::Float(a), DictValue::Int(b)) => DictValue::Float(*a + *b as f64),
        (DictValue::Float(a), DictValue::Float(b)) => DictValue::Float(*a + *b),
        _ => DictValue::Float(0.0),
    }
}

pub(crate) fn expire_from_exptime(exptime: f64, now: u64) -> Result<u64, &'static str> {
    if !exptime.is_finite() {
        return Err("invalid exptime");
    }
    if exptime <= 0.0 {
        return Ok(0);
    }
    let ms = (exptime * 1000.0).round();
    if !ms.is_finite() || ms < 1.0 || ms > MAX_EXPTIME_MS as f64 {
        return Err("invalid exptime");
    }
    now.checked_add(ms as u64).ok_or("invalid exptime")
}

pub(crate) fn validate_key(key: &[u8]) -> Result<(), &'static str> {
    if key.is_empty() {
        Err("empty key")
    } else if key.len() > MAX_KEY_BYTES {
        Err("key too long")
    } else {
        Ok(())
    }
}

pub(crate) fn enumeration_limit(max: Option<i64>, hard: usize) -> usize {
    match max {
        Some(n) if n > 0 => (n as usize).min(hard),
        _ => hard,
    }
}

pub fn validate_zone_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        Err("empty shared dict name")
    } else if name.len() > MAX_ZONE_NAME_BYTES {
        Err("shared dict name too long")
    } else {
        Ok(())
    }
}

pub(crate) fn now_ms() -> u64 {
    #[cfg(not(ngx_features = "rustc-test"))]
    {
        let tp = nginx_sys::ngx_timeofday();
        (tp.sec as u64)
            .saturating_mul(1000)
            .saturating_add(tp.msec as u64)
    }
    #[cfg(ngx_features = "rustc-test")]
    process_now_ms()
}

#[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
fn process_now_ms() -> u64 {
    if let Some(forced) = *clock_slot().lock().expect("clock mutex") {
        return forced;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
mod process_local {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    const LIST_NODE_OVERHEAD: usize = 16;

    enum ProcessPayload {
        Scalar(DictValue),
        List(VecDeque<DictValue>),
    }

    struct ProcessNode {
        key: Vec<u8>,
        payload: ProcessPayload,
        flags: u32,
        expire_ms: u64,
        prev: Option<usize>,
        next: Option<usize>,
    }

    pub(super) struct ProcessDict {
        pub(super) capacity: usize,
        used: usize,
        map: HashMap<Vec<u8>, usize>,
        nodes: Vec<ProcessNode>,
        free: Vec<usize>,
        mru: Option<usize>,
        lru: Option<usize>,
    }

    struct ZoneReg {
        dict: Arc<Mutex<ProcessDict>>,
        generation: u64,
    }

    static NEXT_GEN: AtomicU64 = AtomicU64::new(1);

    impl ProcessDict {
        fn new(capacity: usize) -> Self {
            Self {
                capacity,
                used: 0,
                map: HashMap::new(),
                nodes: Vec::new(),
                free: Vec::new(),
                mru: None,
                lru: None,
            }
        }

        fn entry_size(key: &[u8], payload: &ProcessPayload) -> usize {
            match payload {
                ProcessPayload::Scalar(value) => NODE_OVERHEAD + key.len() + value.stored_len(),
                ProcessPayload::List(items) => {
                    NODE_OVERHEAD
                        + key.len()
                        + LIST_NODE_OVERHEAD
                        + items
                            .iter()
                            .map(|item| LIST_NODE_OVERHEAD + item.stored_len())
                            .sum::<usize>()
                }
            }
        }

        fn item_size(value: &DictValue) -> usize {
            LIST_NODE_OVERHEAD + value.stored_len()
        }

        fn is_list(node: &ProcessNode) -> bool {
            matches!(node.payload, ProcessPayload::List(_))
        }

        fn unlink(&mut self, idx: usize) {
            let prev = self.nodes[idx].prev;
            let next = self.nodes[idx].next;
            if let Some(prev) = prev {
                self.nodes[prev].next = next;
            } else {
                self.mru = next;
            }
            if let Some(next) = next {
                self.nodes[next].prev = prev;
            } else {
                self.lru = prev;
            }
            self.nodes[idx].prev = None;
            self.nodes[idx].next = None;
        }

        fn push_mru(&mut self, idx: usize) {
            self.nodes[idx].prev = None;
            self.nodes[idx].next = self.mru;
            if let Some(mru) = self.mru {
                self.nodes[mru].prev = Some(idx);
            } else {
                self.lru = Some(idx);
            }
            self.mru = Some(idx);
        }

        fn touch(&mut self, idx: usize) {
            self.unlink(idx);
            self.push_mru(idx);
        }

        fn remove_at(&mut self, idx: usize) {
            let key = self.nodes[idx].key.clone();
            let size = Self::entry_size(&key, &self.nodes[idx].payload);
            self.unlink(idx);
            self.map.remove(&key);
            self.used = self.used.saturating_sub(size);
            self.free.push(idx);
        }

        fn expired(node: &ProcessNode, now: u64) -> bool {
            node.expire_ms != 0 && node.expire_ms <= now
        }

        fn lookup_live(&mut self, key: &[u8], now: u64) -> Option<usize> {
            let idx = *self.map.get(key)?;
            if Self::expired(&self.nodes[idx], now) {
                None
            } else {
                Some(idx)
            }
        }

        fn alloc_slot(&mut self, node: ProcessNode) -> usize {
            if let Some(idx) = self.free.pop() {
                self.nodes[idx] = node;
                idx
            } else {
                let idx = self.nodes.len();
                self.nodes.push(node);
                idx
            }
        }

        fn evict_skipping(&mut self, now: u64, skip: Option<&[u8]>) -> Option<bool> {
            let mut idx = self.lru;
            while let Some(cur) = idx {
                if skip.is_some_and(|key| self.nodes[cur].key == key) {
                    idx = self.nodes[cur].prev;
                    continue;
                }
                let forcible = !Self::expired(&self.nodes[cur], now);
                self.remove_at(cur);
                return Some(forcible);
            }
            None
        }

        fn make_room(
            &mut self,
            need: usize,
            safe: bool,
            now: u64,
            skip: Option<&[u8]>,
        ) -> Result<bool, StoreResult> {
            let mut forcible = false;
            let mut attempts = 0;
            while self.used + need > self.capacity {
                if safe {
                    return Err(store_err("no memory"));
                }
                if attempts >= MAX_EVICT {
                    return Err(store_err("no memory"));
                }
                match self.evict_skipping(now, skip) {
                    Some(true) => forcible = true,
                    Some(false) => {}
                    None => return Err(store_err("no memory")),
                }
                attempts += 1;
            }
            Ok(forcible)
        }

        #[allow(clippy::too_many_arguments)]
        fn insert_new(
            &mut self,
            key: Vec<u8>,
            value: DictValue,
            flags: u32,
            expire_ms: u64,
            safe: bool,
            now: u64,
            skip: Option<&[u8]>,
        ) -> StoreResult {
            let need = Self::entry_size(&key, &ProcessPayload::Scalar(value.clone()));
            let forcible = match self.make_room(need, safe, now, skip) {
                Ok(forcible) => forcible,
                Err(err) => return err,
            };
            if self.used + need > self.capacity {
                return store_err("no memory");
            }
            let idx = self.alloc_slot(ProcessNode {
                key: key.clone(),
                payload: ProcessPayload::Scalar(value),
                flags,
                expire_ms,
                prev: None,
                next: None,
            });
            self.map.insert(key, idx);
            self.used += need;
            self.push_mru(idx);
            store_ok(forcible)
        }

        pub(super) fn store(
            &mut self,
            key: &[u8],
            value: Option<DictValue>,
            exptime: f64,
            flags: u32,
            mode: StoreMode,
            now: u64,
        ) -> StoreResult {
            if let Err(err) = validate_key(key) {
                return store_err(err);
            }
            let Some(value) = value else {
                match mode {
                    StoreMode::Add | StoreMode::SafeAdd | StoreMode::Replace => {
                        return store_err("attempt to add or replace nil values");
                    }
                    StoreMode::Set | StoreMode::SafeSet => {
                        self.delete(key);
                        return store_ok(false);
                    }
                }
            };
            if value.stored_len() > MAX_VALUE_BYTES {
                return store_err("value too long");
            }
            let expire_ms = match expire_from_exptime(exptime, now) {
                Ok(ms) => ms,
                Err(err) => return store_err(err),
            };
            let live = self.lookup_live(key, now);
            match mode {
                StoreMode::Add | StoreMode::SafeAdd => {
                    if live.is_some() {
                        return store_err("exists");
                    }
                }
                StoreMode::Replace => {
                    if live.is_none() {
                        return store_err("not found");
                    }
                }
                StoreMode::Set | StoreMode::SafeSet => {}
            }
            let safe = matches!(mode, StoreMode::SafeSet | StoreMode::SafeAdd);
            if let Some(idx) = live.or_else(|| self.map.get(key).copied()) {
                if Self::is_list(&self.nodes[idx]) {
                    self.remove_at(idx);
                    return self.insert_new(key.to_vec(), value, flags, expire_ms, safe, now, None);
                }
                let old_size = Self::entry_size(&self.nodes[idx].key, &self.nodes[idx].payload);
                let new_size = Self::entry_size(key, &ProcessPayload::Scalar(value.clone()));
                if old_size == new_size {
                    self.nodes[idx].payload = ProcessPayload::Scalar(value);
                    self.nodes[idx].flags = flags;
                    self.nodes[idx].expire_ms = expire_ms;
                    self.touch(idx);
                    return store_ok(false);
                }
                let extra = new_size.saturating_sub(old_size);
                let forcible = match self.make_room(extra, safe, now, Some(key)) {
                    Ok(forcible) => forcible,
                    Err(err) => return err,
                };
                self.remove_at(idx);
                let mut result =
                    self.insert_new(key.to_vec(), value, flags, expire_ms, true, now, None);
                if result.ok {
                    result.forcible = forcible;
                }
                return result;
            }
            self.insert_new(key.to_vec(), value, flags, expire_ms, safe, now, None)
        }

        pub(super) fn get(&mut self, key: &[u8], stale: bool, now: u64) -> GetResult {
            if let Err(err) = validate_key(key) {
                return empty_get(err);
            }
            let Some(&idx) = self.map.get(key) else {
                return GetResult {
                    value: None,
                    flags: 0,
                    stale: false,
                    error: None,
                };
            };
            let expired = Self::expired(&self.nodes[idx], now);
            if expired && !stale {
                return GetResult {
                    value: None,
                    flags: 0,
                    stale: false,
                    error: None,
                };
            }
            if Self::is_list(&self.nodes[idx]) {
                return GetResult {
                    value: None,
                    flags: 0,
                    stale: expired,
                    error: Some("value is a list"),
                };
            }
            if !expired {
                self.touch(idx);
            }
            let ProcessPayload::Scalar(value) = &self.nodes[idx].payload else {
                return GetResult {
                    value: None,
                    flags: 0,
                    stale: expired,
                    error: Some("value is a list"),
                };
            };
            GetResult {
                value: Some(value.clone()),
                flags: self.nodes[idx].flags,
                stale: expired,
                error: None,
            }
        }

        pub(super) fn delete(&mut self, key: &[u8]) -> bool {
            let Some(&idx) = self.map.get(key) else {
                return false;
            };
            self.remove_at(idx);
            true
        }

        pub(super) fn incr(
            &mut self,
            key: &[u8],
            delta: DictValue,
            init: Option<DictValue>,
            init_ttl: f64,
            now: u64,
        ) -> StoreResult {
            if let Err(err) = validate_key(key) {
                return store_err(err);
            }
            let Some(delta) = delta.as_number() else {
                return store_err("not a number");
            };
            if let Some(idx) = self.lookup_live(key, now) {
                let ProcessPayload::Scalar(current_value) = &self.nodes[idx].payload else {
                    return store_err("not a number");
                };
                let Some(current) = current_value.as_number() else {
                    return store_err("not a number");
                };
                let current_len = current_value.stored_len();
                let next = add_numbers(&current, &delta);
                if current_len == next.stored_len() {
                    self.nodes[idx].payload = ProcessPayload::Scalar(next.clone());
                    self.touch(idx);
                    return StoreResult {
                        ok: true,
                        error: None,
                        forcible: false,
                        value: Some(next),
                    };
                }
                let flags = self.nodes[idx].flags;
                let expire_ms = self.nodes[idx].expire_ms;
                let extra = Self::entry_size(key, &ProcessPayload::Scalar(next.clone()))
                    .saturating_sub(Self::entry_size(key, &self.nodes[idx].payload));
                let forcible = match self.make_room(extra, false, now, Some(key)) {
                    Ok(forcible) => forcible,
                    Err(err) => return err,
                };
                self.remove_at(idx);
                let mut result = self.insert_new(
                    key.to_vec(),
                    next.clone(),
                    flags,
                    expire_ms,
                    true,
                    now,
                    None,
                );
                result.value = Some(next);
                if result.ok {
                    result.forcible = forcible;
                }
                return result;
            }
            let Some(init) = init.and_then(|value| value.as_number()) else {
                return store_err("not found");
            };
            let next = add_numbers(&init, &delta);
            let expire_ms = match expire_from_exptime(init_ttl, now) {
                Ok(ms) => ms,
                Err(err) => return store_err(err),
            };
            let mut result =
                self.insert_new(key.to_vec(), next.clone(), 0, expire_ms, false, now, None);
            result.value = Some(next);
            result
        }

        pub(super) fn ttl(&mut self, key: &[u8], now: u64) -> Result<f64, &'static str> {
            validate_key(key)?;
            let idx = self.lookup_live(key, now).ok_or("not found")?;
            let expire_ms = self.nodes[idx].expire_ms;
            if expire_ms == 0 {
                return Ok(0.0);
            }
            Ok((expire_ms.saturating_sub(now) as f64) / 1000.0)
        }

        pub(super) fn expire(&mut self, key: &[u8], exptime: f64, now: u64) -> StoreResult {
            if let Err(err) = validate_key(key) {
                return store_err(err);
            }
            let Some(idx) = self.lookup_live(key, now) else {
                return store_err("not found");
            };
            match expire_from_exptime(exptime, now) {
                Ok(ms) => {
                    self.nodes[idx].expire_ms = ms;
                    store_ok(false)
                }
                Err(err) => store_err(err),
            }
        }

        pub(super) fn free_space(&self) -> usize {
            self.capacity.saturating_sub(self.used)
        }

        fn validate_list_item(value: &DictValue) -> Result<(), &'static str> {
            match value {
                DictValue::Bool(_) => Err("bad value type"),
                DictValue::String(text) if text.len() > MAX_VALUE_BYTES => Err("value too long"),
                DictValue::Bytes(bytes) if bytes.len() > MAX_VALUE_BYTES => Err("value too long"),
                _ => Ok(()),
            }
        }

        fn insert_list(&mut self, key: Vec<u8>, first: DictValue, _now: u64) -> StoreResult {
            let mut items = VecDeque::new();
            items.push_back(first);
            let payload = ProcessPayload::List(items);
            let need = Self::entry_size(&key, &payload);
            if self.used + need > self.capacity {
                return store_err("no memory");
            }
            let idx = self.alloc_slot(ProcessNode {
                key: key.clone(),
                payload,
                flags: 0,
                expire_ms: 0,
                prev: None,
                next: None,
            });
            self.map.insert(key, idx);
            self.used += need;
            self.push_mru(idx);
            StoreResult {
                ok: true,
                error: None,
                forcible: false,
                value: Some(DictValue::Int(1)),
            }
        }

        pub(super) fn list_push(
            &mut self,
            key: &[u8],
            value: DictValue,
            left: bool,
            now: u64,
        ) -> StoreResult {
            if let Err(err) = validate_key(key) {
                return store_err(err);
            }
            if let Err(err) = Self::validate_list_item(&value) {
                return store_err(err);
            }
            if let Some(&idx) = self.map.get(key) {
                let expired = Self::expired(&self.nodes[idx], now);
                if !Self::is_list(&self.nodes[idx]) {
                    if expired {
                        self.remove_at(idx);
                        return self.insert_list(key.to_vec(), value, now);
                    }
                    return store_err("value not a list");
                }
                if expired {
                    let old = Self::entry_size(&self.nodes[idx].key, &self.nodes[idx].payload);
                    self.nodes[idx].payload = ProcessPayload::List(VecDeque::new());
                    self.nodes[idx].expire_ms = 0;
                    self.nodes[idx].flags = 0;
                    self.used = self.used.saturating_sub(old)
                        + Self::entry_size(&self.nodes[idx].key, &self.nodes[idx].payload);
                }
                let ProcessPayload::List(items) = &self.nodes[idx].payload else {
                    return store_err("value not a list");
                };
                if items.len() >= MAX_LIST_LEN {
                    return store_err("list too long");
                }
                let extra = Self::item_size(&value);
                if self.used + extra > self.capacity {
                    if matches!(&self.nodes[idx].payload, ProcessPayload::List(items) if items.is_empty())
                    {
                        self.remove_at(idx);
                    }
                    return store_err("no memory");
                }
                let ProcessPayload::List(items) = &mut self.nodes[idx].payload else {
                    return store_err("value not a list");
                };
                if left {
                    items.push_front(value);
                } else {
                    items.push_back(value);
                }
                let len = items.len() as i64;
                self.used += extra;
                self.touch(idx);
                return StoreResult {
                    ok: true,
                    error: None,
                    forcible: false,
                    value: Some(DictValue::Int(len)),
                };
            }
            self.insert_list(key.to_vec(), value, now)
        }

        pub(super) fn list_pop(&mut self, key: &[u8], left: bool, now: u64) -> GetResult {
            if let Err(err) = validate_key(key) {
                return empty_get(err);
            }
            let Some(&idx) = self.map.get(key) else {
                return GetResult {
                    value: None,
                    flags: 0,
                    stale: false,
                    error: None,
                };
            };
            if Self::expired(&self.nodes[idx], now) {
                return GetResult {
                    value: None,
                    flags: 0,
                    stale: false,
                    error: None,
                };
            }
            if !Self::is_list(&self.nodes[idx]) {
                return GetResult {
                    value: None,
                    flags: 0,
                    stale: false,
                    error: Some("value not a list"),
                };
            }
            let ProcessPayload::List(items) = &mut self.nodes[idx].payload else {
                return GetResult {
                    value: None,
                    flags: 0,
                    stale: false,
                    error: Some("value not a list"),
                };
            };
            let popped = if left {
                items.pop_front()
            } else {
                items.pop_back()
            };
            let Some(value) = popped else {
                self.remove_at(idx);
                return GetResult {
                    value: None,
                    flags: 0,
                    stale: false,
                    error: None,
                };
            };
            let extra = Self::item_size(&value);
            self.used = self.used.saturating_sub(extra);
            if matches!(&self.nodes[idx].payload, ProcessPayload::List(items) if items.is_empty()) {
                self.remove_at(idx);
            } else {
                self.touch(idx);
            }
            GetResult {
                value: Some(value),
                flags: 0,
                stale: false,
                error: None,
            }
        }

        pub(super) fn llen(&mut self, key: &[u8], now: u64) -> GetResult {
            if let Err(err) = validate_key(key) {
                return empty_get(err);
            }
            let Some(&idx) = self.map.get(key) else {
                return GetResult {
                    value: Some(DictValue::Int(0)),
                    flags: 0,
                    stale: false,
                    error: None,
                };
            };
            if Self::expired(&self.nodes[idx], now) {
                return GetResult {
                    value: Some(DictValue::Int(0)),
                    flags: 0,
                    stale: false,
                    error: None,
                };
            }
            match &self.nodes[idx].payload {
                ProcessPayload::List(items) => {
                    let len = items.len() as i64;
                    self.touch(idx);
                    GetResult {
                        value: Some(DictValue::Int(len)),
                        flags: 0,
                        stale: false,
                        error: None,
                    }
                }
                ProcessPayload::Scalar(_) => GetResult {
                    value: None,
                    flags: 0,
                    stale: false,
                    error: Some("value not a list"),
                },
            }
        }

        pub(super) fn flush_all(&mut self, now: u64) {
            let _ = self.flush_expired(Some(MAX_FLUSH_BATCH as i64), now);
            let mut visits = 0usize;
            let mut idx = self.lru;
            while let Some(cur) = idx {
                if visits >= MAX_FLUSH_BATCH {
                    break;
                }
                visits += 1;
                let prev = self.nodes[cur].prev;
                if !Self::expired(&self.nodes[cur], now) {
                    self.nodes[cur].expire_ms = 1;
                }
                idx = prev;
            }
        }

        pub(super) fn flush_expired(&mut self, max: Option<i64>, now: u64) -> i64 {
            let limit = enumeration_limit(max, MAX_FLUSH_BATCH);
            let mut freed = 0i64;
            let mut visits = 0usize;
            let mut idx = self.lru;
            while let Some(cur) = idx {
                if visits >= MAX_FLUSH_BATCH || (freed as usize) >= limit {
                    break;
                }
                visits += 1;
                let prev = self.nodes[cur].prev;
                if Self::expired(&self.nodes[cur], now) {
                    self.remove_at(cur);
                    freed += 1;
                }
                idx = prev;
            }
            freed
        }

        pub(super) fn get_keys(&mut self, max: Option<i64>, now: u64) -> Vec<Vec<u8>> {
            let limit = enumeration_limit(max, MAX_GET_KEYS);
            let mut keys = Vec::new();
            let mut visits = 0usize;
            let mut idx = self.lru;
            while let Some(cur) = idx {
                if keys.len() >= limit || visits >= MAX_GET_KEYS {
                    break;
                }
                visits += 1;
                if !Self::expired(&self.nodes[cur], now) {
                    keys.push(self.nodes[cur].key.clone());
                }
                idx = self.nodes[cur].prev;
            }
            keys
        }
    }

    fn process_zones() -> &'static Mutex<HashMap<String, ZoneReg>> {
        static ZONES: OnceLock<Mutex<HashMap<String, ZoneReg>>> = OnceLock::new();
        ZONES.get_or_init(|| Mutex::new(HashMap::new()))
    }

    static CLOCK: OnceLock<Mutex<Option<u64>>> = OnceLock::new();

    pub(super) fn clock_slot() -> &'static Mutex<Option<u64>> {
        CLOCK.get_or_init(|| Mutex::new(None))
    }

    pub fn install_process_zone(name: &str, capacity: usize) {
        let generation = NEXT_GEN.fetch_add(1, Ordering::Relaxed);
        process_zones().lock().expect("process zone mutex").insert(
            name.to_string(),
            ZoneReg {
                dict: Arc::new(Mutex::new(ProcessDict::new(capacity))),
                generation,
            },
        );
    }

    pub fn reset_process_zones() {
        process_zones().lock().expect("process zone mutex").clear();
        *clock_slot().lock().expect("clock mutex") = None;
    }

    pub fn set_clock_ms(now: Option<u64>) {
        *clock_slot().lock().expect("clock mutex") = now;
    }

    fn process_entry(name: &str) -> Option<ZoneReg> {
        let zones = process_zones().lock().expect("process zone mutex");
        zones.get(name).map(|entry| ZoneReg {
            dict: Arc::clone(&entry.dict),
            generation: entry.generation,
        })
    }

    pub(super) fn process_gen(name: &str) -> Option<u64> {
        process_entry(name).map(|entry| entry.generation)
    }

    pub(super) fn with_process<T>(
        name: &str,
        f: impl FnOnce(&mut ProcessDict) -> T,
    ) -> Result<T, &'static str> {
        let zone = process_entry(name).ok_or("not found")?;
        let mut guard = zone.dict.lock().expect("process dict mutex");
        Ok(f(&mut guard))
    }

    pub(super) fn has_process(name: &str) -> bool {
        process_entry(name).is_some()
    }
}

#[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
use process_local::clock_slot;

/// Install a process-local zone used by unit tests (never compiled into NGINX workers).
#[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
pub use process_local::{install_process_zone, reset_process_zones, set_clock_ms};

pub fn open_handle(name: &str) -> Result<SharedDictHandle, &'static str> {
    validate_zone_name(name)?;
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if let Some(generation) = process_local::process_gen(name) {
        return Ok(SharedDictHandle {
            name: name.to_string(),
            generation,
        });
    }
    if shm_zone_exists(name) {
        return Ok(SharedDictHandle {
            name: name.to_string(),
            generation: 0,
        });
    }
    Err("not found")
}

pub fn require_handle(handle: &SharedDictHandle) -> Result<(), &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if let Some(generation) = process_local::process_gen(&handle.name) {
        return if generation == handle.generation {
            Ok(())
        } else {
            Err("not found")
        };
    }
    if shm_zone_exists(&handle.name) {
        return Ok(());
    }
    Err("not found")
}

/// Ensure `name` names a configured zone.
pub fn require_zone(name: &str) -> Result<(), &'static str> {
    open_handle(name).map(|_| ())
}

pub fn dict_get_for(
    handle: &SharedDictHandle,
    key: &[u8],
    stale: bool,
) -> Result<GetResult, &'static str> {
    require_handle(handle)?;
    dict_get(&handle.name, key, stale)
}

pub fn dict_get(name: &str, key: &[u8], stale: bool) -> Result<GetResult, &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| dict.get(key, stale, process_now_ms()));
    }
    shm_backend::get(name, key, stale)
}

pub fn dict_store(
    name: &str,
    key: &[u8],
    value: Option<DictValue>,
    exptime: f64,
    flags: u32,
    mode: StoreMode,
) -> Result<StoreResult, &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| {
            dict.store(key, value, exptime, flags, mode, process_now_ms())
        });
    }
    shm_backend::store(name, key, value, exptime, flags, mode)
}

pub fn dict_delete(name: &str, key: &[u8]) -> Result<bool, &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| dict.delete(key));
    }
    shm_backend::delete(name, key)
}

pub fn dict_incr(
    name: &str,
    key: &[u8],
    delta: DictValue,
    init: Option<DictValue>,
    init_ttl: f64,
) -> Result<StoreResult, &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| {
            dict.incr(key, delta, init, init_ttl, process_now_ms())
        });
    }
    shm_backend::incr(name, key, delta, init, init_ttl)
}

pub fn dict_ttl(name: &str, key: &[u8]) -> Result<Result<f64, &'static str>, &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| dict.ttl(key, process_now_ms()));
    }
    shm_backend::ttl(name, key)
}

pub fn dict_expire(name: &str, key: &[u8], exptime: f64) -> Result<StoreResult, &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| {
            dict.expire(key, exptime, process_now_ms())
        });
    }
    shm_backend::expire(name, key, exptime)
}

pub fn dict_capacity(name: &str) -> Result<usize, &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| dict.capacity);
    }
    shm_backend::capacity(name)
}

pub fn dict_free_space(name: &str) -> Result<usize, &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| dict.free_space());
    }
    shm_backend::free_space(name)
}

pub fn dict_lpush(name: &str, key: &[u8], value: DictValue) -> Result<StoreResult, &'static str> {
    dict_list_push(name, key, value, true)
}

pub fn dict_rpush(name: &str, key: &[u8], value: DictValue) -> Result<StoreResult, &'static str> {
    dict_list_push(name, key, value, false)
}

fn dict_list_push(
    name: &str,
    key: &[u8],
    value: DictValue,
    left: bool,
) -> Result<StoreResult, &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| {
            dict.list_push(key, value, left, process_now_ms())
        });
    }
    shm_backend::list_push(name, key, value, left)
}

pub fn dict_lpop(name: &str, key: &[u8]) -> Result<GetResult, &'static str> {
    dict_list_pop(name, key, true)
}

pub fn dict_rpop(name: &str, key: &[u8]) -> Result<GetResult, &'static str> {
    dict_list_pop(name, key, false)
}

fn dict_list_pop(name: &str, key: &[u8], left: bool) -> Result<GetResult, &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| {
            dict.list_pop(key, left, process_now_ms())
        });
    }
    shm_backend::list_pop(name, key, left)
}

pub fn dict_llen(name: &str, key: &[u8]) -> Result<GetResult, &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| dict.llen(key, process_now_ms()));
    }
    shm_backend::llen(name, key)
}

pub fn dict_flush_all(name: &str) -> Result<(), &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| dict.flush_all(process_now_ms()));
    }
    shm_backend::flush_all(name)
}

pub fn dict_flush_expired(name: &str, max: Option<i64>) -> Result<i64, &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| dict.flush_expired(max, process_now_ms()));
    }
    shm_backend::flush_expired(name, max)
}

pub fn dict_get_keys(name: &str, max: Option<i64>) -> Result<Vec<Vec<u8>>, &'static str> {
    #[cfg(any(debug_assertions, ngx_features = "rustc-test"))]
    if process_local::has_process(name) {
        return process_local::with_process(name, |dict| dict.get_keys(max, process_now_ms()));
    }
    shm_backend::get_keys(name, max)
}

fn shm_zone_exists(name: &str) -> bool {
    shm_backend::zone_exists(name)
}

#[cfg(ngx_features = "rustc-test")]
mod shm_backend {
    use super::{DictValue, GetResult, StoreMode, StoreResult};

    pub(super) fn zone_exists(_: &str) -> bool {
        false
    }
    pub(super) fn get(_: &str, _: &[u8], _: bool) -> Result<GetResult, &'static str> {
        Err("not found")
    }
    pub(super) fn store(
        _: &str,
        _: &[u8],
        _: Option<DictValue>,
        _: f64,
        _: u32,
        _: StoreMode,
    ) -> Result<StoreResult, &'static str> {
        Err("not found")
    }
    pub(super) fn delete(_: &str, _: &[u8]) -> Result<bool, &'static str> {
        Err("not found")
    }
    pub(super) fn incr(
        _: &str,
        _: &[u8],
        _: DictValue,
        _: Option<DictValue>,
        _: f64,
    ) -> Result<StoreResult, &'static str> {
        Err("not found")
    }
    pub(super) fn ttl(_: &str, _: &[u8]) -> Result<Result<f64, &'static str>, &'static str> {
        Err("not found")
    }
    pub(super) fn expire(_: &str, _: &[u8], _: f64) -> Result<StoreResult, &'static str> {
        Err("not found")
    }
    pub(super) fn capacity(_: &str) -> Result<usize, &'static str> {
        Err("not found")
    }
    pub(super) fn free_space(_: &str) -> Result<usize, &'static str> {
        Err("not found")
    }
    pub(super) fn list_push(
        _: &str,
        _: &[u8],
        _: DictValue,
        _: bool,
    ) -> Result<StoreResult, &'static str> {
        Err("not found")
    }
    pub(super) fn list_pop(_: &str, _: &[u8], _: bool) -> Result<GetResult, &'static str> {
        Err("not found")
    }
    pub(super) fn llen(_: &str, _: &[u8]) -> Result<GetResult, &'static str> {
        Err("not found")
    }
    pub(super) fn flush_all(_: &str) -> Result<(), &'static str> {
        Err("not found")
    }
    pub(super) fn flush_expired(_: &str, _: Option<i64>) -> Result<i64, &'static str> {
        Err("not found")
    }
    pub(super) fn get_keys(_: &str, _: Option<i64>) -> Result<Vec<Vec<u8>>, &'static str> {
        Err("not found")
    }
}

#[cfg(not(ngx_features = "rustc-test"))]
#[path = "shared_dict_shm.rs"]
mod shm_backend;

#[cfg(not(ngx_features = "rustc-test"))]
pub(crate) use shm_backend::attach_shm_zone;
