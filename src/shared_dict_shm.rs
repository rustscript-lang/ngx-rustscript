//! NGINX shm/slab/rbtree backend for [`crate::shared_dict`].
//!
//! Layout and algorithms follow OpenResty `ngx_http_lua_shdict.c` against
//! NGINX 1.31.4 `ngx_rbtree` / `ngx_queue` / `ngx_slab`. Compiled only in the
//! production cdylib (`not(ngx_features = "rustc-test")`).

use super::{
    DictValue, GetResult, MAX_EVICT, MAX_FLUSH_BATCH, MAX_GET_KEYS, MAX_LIST_LEN, MAX_VALUE_BYTES,
    StoreMode, StoreResult, add_numbers, empty_get, enumeration_limit, expire_from_exptime,
    nginx_memn2cmp, ngx_crc32_short, now_ms, store_err, store_ok, validate_key,
};
use core::mem::{self, offset_of};
use core::ptr::{self, NonNull};
use core::slice;
use nginx_sys::{
    ngx_conf_t, ngx_cycle, ngx_cycle_t, ngx_int_t, ngx_list_part_t, ngx_pagesize, ngx_pcalloc,
    ngx_queue_empty, ngx_queue_init, ngx_queue_insert_after, ngx_queue_insert_before,
    ngx_queue_remove, ngx_queue_t, ngx_rbt_red, ngx_rbtree_delete, ngx_rbtree_init,
    ngx_rbtree_insert, ngx_rbtree_key_t, ngx_rbtree_node_t, ngx_rbtree_t, ngx_shm_zone_t,
    ngx_shmtx_lock, ngx_shmtx_unlock, ngx_slab_alloc_locked, ngx_slab_free_locked, ngx_slab_pool_t,
    ngx_str_t,
};
use ngx::core::Status;

const T_BOOL: u8 = 1;
const T_INT: u8 = 2;
const T_FLOAT: u8 = 3;
const T_STRING: u8 = 4;
const T_BYTES: u8 = 5;
const T_LIST: u8 = 6;
/// NGINX `NGX_ALIGNMENT` on the supported 1.31.4 x86_64 tree.
const SHDICT_ALIGNMENT: usize = 16;

/// Conf-pool context stored in `ngx_shm_zone_t.data` (OpenResty `shdict_ctx`).
#[repr(C)]
pub(crate) struct ShdictZoneCtx {
    sh: *mut ShdictShctx,
    shpool: *mut ngx_slab_pool_t,
    name: ngx_str_t,
}

/// Shared header in slab. LRU sentinel is a dedicated `ngx_queue_t`.
#[repr(C)]
struct ShdictShctx {
    rbtree: ngx_rbtree_t,
    sentinel: ngx_rbtree_node_t,
    lru_queue: ngx_queue_t,
}

/// Overlay starting at `ngx_rbtree_node_t.color`, matching OpenResty `shdict_node_t`.
#[repr(C)]
struct ShdictNode {
    color: u8,
    value_type: u8,
    key_len: u16,
    value_len: u32,
    expires: u64,
    queue: ngx_queue_t,
    user_flags: u32,
    data: [u8; 0],
}

#[repr(C)]
struct ShdictListNode {
    queue: ngx_queue_t,
    value_len: u32,
    value_type: u8,
    data: [u8; 0],
}

fn ngx_str_eq(s: ngx_str_t, name: &str) -> bool {
    if s.len != name.len() {
        return false;
    }
    if s.data.is_null() {
        return name.is_empty();
    }
    // SAFETY: `s` is an NGINX string from the live cycle shared-memory list.
    let bytes = unsafe { slice::from_raw_parts(s.data, s.len) };
    bytes == name.as_bytes()
}

fn module_tag() -> *mut core::ffi::c_void {
    ptr::addr_of_mut!(crate::ngx_http_rustscript_module).cast()
}

unsafe fn cycle_ref() -> Option<&'static ngx_cycle_t> {
    // SAFETY: `ngx_cycle` is the process cycle pointer; null means tests/no nginx.
    let ptr = unsafe { ngx_cycle };
    if ptr.is_null() {
        None
    } else {
        // SAFETY: NGINX keeps `ngx_cycle` pointing at a live cycle in workers.
        Some(unsafe { &*ptr })
    }
}

fn find_zone(name: &str) -> Option<NonNull<ngx_shm_zone_t>> {
    let cycle = unsafe { cycle_ref()? };
    let mut part: *const ngx_list_part_t = ptr::addr_of!(cycle.shared_memory.part);
    loop {
        if part.is_null() {
            return None;
        }
        // SAFETY: `part` walks NGINX's shared_memory list.
        let part_ref = unsafe { &*part };
        let elts = part_ref.elts.cast::<ngx_shm_zone_t>();
        for i in 0..part_ref.nelts {
            // SAFETY: `i` is in-range for this list part.
            let zone = unsafe { elts.add(i) };
            if zone.is_null() {
                continue;
            }
            // SAFETY: zone entries are live ngx_shm_zone_t values.
            let zone_ref = unsafe { &*zone };
            if zone_ref.tag != module_tag() {
                continue;
            }
            if ngx_str_eq(zone_ref.shm.name, name) {
                return NonNull::new(zone);
            }
        }
        part = part_ref.next;
    }
}

pub(super) fn zone_exists(name: &str) -> bool {
    find_zone(name).is_some()
}

fn node_sd(node: *mut ngx_rbtree_node_t) -> *mut ShdictNode {
    // SAFETY: OpenResty overlays shdict_node at `&node->color`.
    unsafe { ptr::addr_of_mut!((*node).color).cast() }
}

fn sd_rbtree_node(sd: *mut ShdictNode) -> *mut ngx_rbtree_node_t {
    // SAFETY: `sd` was derived from `&node->color`.
    unsafe {
        sd.cast::<u8>()
            .sub(offset_of!(ngx_rbtree_node_t, color))
            .cast()
    }
}

unsafe fn sd_key<'a>(sd: *mut ShdictNode) -> &'a [u8] {
    let len = unsafe { (*sd).key_len as usize };
    let data = unsafe { ptr::addr_of_mut!((*sd).data) }.cast::<u8>();
    unsafe { slice::from_raw_parts(data, len) }
}

unsafe fn sd_value_bytes<'a>(sd: *mut ShdictNode) -> &'a [u8] {
    let key_len = unsafe { (*sd).key_len as usize };
    let value_len = unsafe { (*sd).value_len as usize };
    let data = unsafe { ptr::addr_of_mut!((*sd).data) }.cast::<u8>();
    unsafe { slice::from_raw_parts(data.add(key_len), value_len) }
}

unsafe fn sd_value_bytes_mut<'a>(sd: *mut ShdictNode) -> &'a mut [u8] {
    let key_len = unsafe { (*sd).key_len as usize };
    let value_len = unsafe { (*sd).value_len as usize };
    let data = unsafe { ptr::addr_of_mut!((*sd).data) }.cast::<u8>();
    unsafe { slice::from_raw_parts_mut(data.add(key_len), value_len) }
}

fn value_type(value: &DictValue) -> u8 {
    match value {
        DictValue::Bool(_) => T_BOOL,
        DictValue::Int(_) => T_INT,
        DictValue::Float(_) => T_FLOAT,
        DictValue::String(_) => T_STRING,
        DictValue::Bytes(_) => T_BYTES,
    }
}

fn encode_value(value: &DictValue, dest: &mut [u8]) {
    match value {
        DictValue::Bool(flag) => dest[0] = u8::from(*flag),
        DictValue::Int(number) => dest.copy_from_slice(&number.to_le_bytes()),
        DictValue::Float(number) => dest.copy_from_slice(&number.to_le_bytes()),
        DictValue::String(text) => dest.copy_from_slice(text.as_bytes()),
        DictValue::Bytes(bytes) => dest.copy_from_slice(bytes),
    }
}

fn decode_value(sd: *mut ShdictNode) -> Option<DictValue> {
    let ty = unsafe { (*sd).value_type };
    let raw = unsafe { sd_value_bytes(sd) };
    match ty {
        T_BOOL => Some(DictValue::Bool(raw.first().copied()? != 0)),
        T_INT => {
            let bytes: [u8; 8] = raw.try_into().ok()?;
            Some(DictValue::Int(i64::from_le_bytes(bytes)))
        }
        T_FLOAT => {
            let bytes: [u8; 8] = raw.try_into().ok()?;
            Some(DictValue::Float(f64::from_le_bytes(bytes)))
        }
        T_STRING => Some(DictValue::String(String::from_utf8(raw.to_vec()).ok()?)),
        T_BYTES => Some(DictValue::Bytes(raw.to_vec())),
        _ => None,
    }
}

fn node_alloc_size(key: &[u8], value_len: usize) -> usize {
    offset_of!(ngx_rbtree_node_t, color) + offset_of!(ShdictNode, data) + key.len() + value_len
}

fn align_up(n: usize) -> usize {
    n.wrapping_add(SHDICT_ALIGNMENT - 1) & !(SHDICT_ALIGNMENT - 1)
}

fn is_list(sd: *mut ShdictNode) -> bool {
    unsafe { (*sd).value_type == T_LIST }
}

fn list_entry_size(key: &[u8]) -> usize {
    align_up(
        offset_of!(ngx_rbtree_node_t, color)
            + offset_of!(ShdictNode, data)
            + key.len()
            + mem::size_of::<ngx_queue_t>(),
    )
}

fn list_item_alloc_size(value_len: usize) -> usize {
    offset_of!(ShdictListNode, data) + value_len
}

fn list_queue(sd: *mut ShdictNode) -> *mut ngx_queue_t {
    unsafe {
        let p = ptr::addr_of_mut!((*sd).data)
            .cast::<u8>()
            .add((*sd).key_len as usize) as usize;
        align_up(p) as *mut ngx_queue_t
    }
}

fn validate_list_item(value: &DictValue) -> Result<(), &'static str> {
    match value {
        DictValue::Bool(_) => Err("bad value type"),
        DictValue::String(text) if text.len() > MAX_VALUE_BYTES => Err("value too long"),
        DictValue::Bytes(bytes) if bytes.len() > MAX_VALUE_BYTES => Err("value too long"),
        _ => Ok(()),
    }
}

fn decode_raw(ty: u8, raw: &[u8]) -> Option<DictValue> {
    match ty {
        T_BOOL => Some(DictValue::Bool(raw.first().copied()? != 0)),
        T_INT => {
            let bytes: [u8; 8] = raw.try_into().ok()?;
            Some(DictValue::Int(i64::from_le_bytes(bytes)))
        }
        T_FLOAT => {
            let bytes: [u8; 8] = raw.try_into().ok()?;
            Some(DictValue::Float(f64::from_le_bytes(bytes)))
        }
        T_STRING => Some(DictValue::String(String::from_utf8(raw.to_vec()).ok()?)),
        T_BYTES => Some(DictValue::Bytes(raw.to_vec())),
        _ => None,
    }
}

fn decode_list_item(lnode: *mut ShdictListNode) -> Option<DictValue> {
    unsafe {
        let ty = (*lnode).value_type;
        let len = (*lnode).value_len as usize;
        let data = ptr::addr_of_mut!((*lnode).data).cast::<u8>();
        decode_raw(ty, slice::from_raw_parts(data, len))
    }
}

unsafe extern "C" fn shdict_rbtree_insert_value(
    mut temp: *mut ngx_rbtree_node_t,
    node: *mut ngx_rbtree_node_t,
    sentinel: *mut ngx_rbtree_node_t,
) {
    // SAFETY: NGINX calls this with live tree nodes while the slab mutex is held.
    unsafe {
        loop {
            let p = if (*node).key < (*temp).key {
                ptr::addr_of_mut!((*temp).left)
            } else if (*node).key > (*temp).key {
                ptr::addr_of_mut!((*temp).right)
            } else {
                let sd = node_sd(temp);
                let nsd = node_sd(node);
                if nginx_memn2cmp(sd_key(nsd), sd_key(sd)) < 0 {
                    ptr::addr_of_mut!((*temp).left)
                } else {
                    ptr::addr_of_mut!((*temp).right)
                }
            };
            if *p == sentinel {
                *p = node;
                (*node).parent = temp;
                (*node).left = sentinel;
                (*node).right = sentinel;
                ngx_rbt_red(node);
                return;
            }
            temp = *p;
        }
    }
}

struct LockedZone {
    zone: NonNull<ngx_shm_zone_t>,
    shpool: NonNull<ngx_slab_pool_t>,
    ctx: NonNull<ShdictZoneCtx>,
}

impl LockedZone {
    fn open(name: &str) -> Result<Self, &'static str> {
        let zone = find_zone(name).ok_or("not found")?;
        let ctx = NonNull::new(unsafe { zone.as_ref().data.cast::<ShdictZoneCtx>() })
            .ok_or("not found")?;
        let shpool = NonNull::new(unsafe { ctx.as_ref().shpool }).ok_or("not found")?;
        unsafe { ngx_shmtx_lock(ptr::addr_of_mut!((*shpool.as_ptr()).mutex)) };
        Ok(Self { zone, shpool, ctx })
    }

    fn sh(&self) -> *mut ShdictShctx {
        unsafe { self.ctx.as_ref().sh }
    }

    fn alloc(&mut self, size: usize) -> *mut u8 {
        unsafe { ngx_slab_alloc_locked(self.shpool.as_ptr(), size).cast() }
    }

    fn free(&mut self, ptr: *mut u8) {
        if !ptr.is_null() {
            unsafe { ngx_slab_free_locked(self.shpool.as_ptr(), ptr.cast()) };
        }
    }

    fn capacity(&self) -> usize {
        unsafe { self.zone.as_ref().shm.size }
    }

    fn pfree(&self) -> usize {
        unsafe { self.shpool.as_ref().pfree }
    }
}

impl Drop for LockedZone {
    fn drop(&mut self) {
        unsafe { ngx_shmtx_unlock(ptr::addr_of_mut!((*self.shpool.as_ptr()).mutex)) };
    }
}

fn free_list_nodes(zone: &mut LockedZone, sd: *mut ShdictNode) {
    if sd.is_null() || !is_list(sd) {
        return;
    }
    unsafe {
        let queue = list_queue(sd);
        let mut q = (*queue).next;
        while q != queue {
            let next = (*q).next;
            zone.free(q.cast());
            q = next;
        }
        ngx_queue_init(queue);
        (*sd).value_len = 0;
    }
}

fn lookup_node(sh: *mut ShdictShctx, key: &[u8]) -> *mut ngx_rbtree_node_t {
    let hash = ngx_crc32_short(key) as ngx_rbtree_key_t;
    unsafe {
        let sentinel = ptr::addr_of_mut!((*sh).sentinel);
        let mut node = (*sh).rbtree.root;
        while node != sentinel {
            if hash < (*node).key {
                node = (*node).left;
            } else if hash > (*node).key {
                node = (*node).right;
            } else {
                let sd = node_sd(node);
                let rc = nginx_memn2cmp(key, sd_key(sd));
                if rc == 0 {
                    return node;
                }
                node = if rc < 0 { (*node).left } else { (*node).right };
            }
        }
    }
    ptr::null_mut()
}

fn expired(sd: *mut ShdictNode, now: u64) -> bool {
    let expires = unsafe { (*sd).expires };
    expires != 0 && expires <= now
}

fn touch_lru(sh: *mut ShdictShctx, sd: *mut ShdictNode) {
    unsafe {
        ngx_queue_remove(ptr::addr_of_mut!((*sd).queue));
        ngx_queue_insert_after(
            ptr::addr_of_mut!((*sh).lru_queue),
            ptr::addr_of_mut!((*sd).queue),
        );
    }
}

/// OpenResty `ngx_http_lua_shdict_expire`. `n == 0` force-deletes the LRU entry.
fn expire_locked(zone: &mut LockedZone, n: usize, skip: *mut ngx_rbtree_node_t) -> usize {
    let sh = zone.sh();
    let now = now_ms();
    let mut n = n;
    let mut freed = 0usize;
    while n < 3 {
        unsafe {
            if ngx_queue_empty(ptr::addr_of_mut!((*sh).lru_queue)) {
                return freed;
            }
            let head = ptr::addr_of_mut!((*sh).lru_queue);
            let mut q = (*head).prev;
            let mut sd = q
                .byte_sub(offset_of!(ShdictNode, queue))
                .cast::<ShdictNode>();
            let mut node = sd_rbtree_node(sd);
            if node == skip {
                q = (*q).prev;
                if q == head {
                    return freed;
                }
                sd = q
                    .byte_sub(offset_of!(ShdictNode, queue))
                    .cast::<ShdictNode>();
                node = sd_rbtree_node(sd);
            }
            if n != 0 {
                if (*sd).expires == 0 {
                    return freed;
                }
                if (*sd).expires > now {
                    return freed;
                }
            }
            n += 1;
            free_list_nodes(zone, sd);
            ngx_queue_remove(q);
            ngx_rbtree_delete(ptr::addr_of_mut!((*sh).rbtree), node);
            zone.free(node.cast());
            freed += 1;
        }
    }
    freed
}

fn delete_node(zone: &mut LockedZone, node: *mut ngx_rbtree_node_t) {
    if node.is_null() {
        return;
    }
    let sh = zone.sh();
    let sd = node_sd(node);
    free_list_nodes(zone, sd);
    unsafe {
        ngx_queue_remove(ptr::addr_of_mut!((*sd).queue));
        ngx_rbtree_delete(ptr::addr_of_mut!((*sh).rbtree), node);
    }
    zone.free(node.cast());
}

fn alloc_node(
    zone: &mut LockedZone,
    size: usize,
    safe: bool,
    skip: *mut ngx_rbtree_node_t,
) -> Result<(*mut ngx_rbtree_node_t, bool), StoreResult> {
    let mut forcible = false;
    let mut attempts = 0;
    loop {
        let ptr = zone.alloc(size);
        if !ptr.is_null() {
            return Ok((ptr.cast(), forcible));
        }
        if safe || attempts >= MAX_EVICT {
            return Err(store_err("no memory"));
        }
        if expire_locked(zone, 0, skip) == 0 {
            return Err(store_err("no memory"));
        }
        forcible = true;
        attempts += 1;
    }
}

fn fill_and_insert(
    zone: &mut LockedZone,
    node: *mut ngx_rbtree_node_t,
    size: usize,
    key: &[u8],
    value: &DictValue,
    flags: u32,
    expire_ms: u64,
) {
    unsafe {
        ptr::write_bytes(node.cast::<u8>(), 0, size);
        let sd = node_sd(node);
        (*sd).value_type = value_type(value);
        (*sd).key_len = key.len() as u16;
        (*sd).value_len = value.stored_len() as u32;
        (*sd).expires = expire_ms;
        (*sd).user_flags = flags;
        let data = ptr::addr_of_mut!((*sd).data).cast::<u8>();
        ptr::copy_nonoverlapping(key.as_ptr(), data, key.len());
        encode_value(
            value,
            slice::from_raw_parts_mut(data.add(key.len()), value.stored_len()),
        );
        (*node).key = ngx_crc32_short(key) as ngx_rbtree_key_t;
        let sh = zone.sh();
        ngx_rbtree_insert(ptr::addr_of_mut!((*sh).rbtree), node);
        ngx_queue_insert_after(
            ptr::addr_of_mut!((*sh).lru_queue),
            ptr::addr_of_mut!((*sd).queue),
        );
    }
}

fn alloc_replace(
    zone: &mut LockedZone,
    key: &[u8],
    value: &DictValue,
    flags: u32,
    expire_ms: u64,
    safe: bool,
    old: *mut ngx_rbtree_node_t,
) -> Result<bool, StoreResult> {
    let size = node_alloc_size(key, value.stored_len());
    let (node, forcible) = alloc_node(zone, size, safe, old)?;
    if !old.is_null() {
        delete_node(zone, old);
    }
    fill_and_insert(zone, node, size, key, value, flags, expire_ms);
    Ok(forcible)
}

fn inplace_update(sd: *mut ShdictNode, value: &DictValue, flags: u32, expire_ms: u64) {
    unsafe {
        (*sd).value_type = value_type(value);
        (*sd).expires = expire_ms;
        (*sd).user_flags = flags;
        encode_value(value, sd_value_bytes_mut(sd));
    }
}

pub(super) fn get(name: &str, key: &[u8], stale: bool) -> Result<GetResult, &'static str> {
    if let Err(err) = validate_key(key) {
        return Ok(empty_get(err));
    }
    let mut zone = LockedZone::open(name)?;
    let _ = expire_locked(&mut zone, 1, ptr::null_mut());
    let sh = zone.sh();
    let node = lookup_node(sh, key);
    if node.is_null() {
        return Ok(GetResult {
            value: None,
            flags: 0,
            stale: false,
            error: None,
        });
    }
    let sd = node_sd(node);
    let now = now_ms();
    let is_expired = expired(sd, now);
    if is_expired && !stale {
        return Ok(GetResult {
            value: None,
            flags: 0,
            stale: false,
            error: None,
        });
    }
    if is_list(sd) {
        return Ok(GetResult {
            value: None,
            flags: 0,
            stale: is_expired,
            error: Some("value is a list"),
        });
    }
    if !is_expired {
        touch_lru(sh, sd);
    }
    Ok(GetResult {
        value: decode_value(sd),
        flags: unsafe { (*sd).user_flags },
        stale: is_expired,
        error: None,
    })
}

pub(super) fn store(
    name: &str,
    key: &[u8],
    value: Option<DictValue>,
    exptime: f64,
    flags: u32,
    mode: StoreMode,
) -> Result<StoreResult, &'static str> {
    if let Err(err) = validate_key(key) {
        return Ok(store_err(err));
    }
    let mut zone = LockedZone::open(name)?;
    let _ = expire_locked(&mut zone, 1, ptr::null_mut());
    let sh = zone.sh();
    let existing = lookup_node(sh, key);
    let live = if existing.is_null() || expired(node_sd(existing), now_ms()) {
        None
    } else {
        Some(existing)
    };
    let Some(value) = value else {
        match mode {
            StoreMode::Add | StoreMode::SafeAdd | StoreMode::Replace => {
                return Ok(store_err("attempt to add or replace nil values"));
            }
            StoreMode::Set | StoreMode::SafeSet => {
                delete_node(&mut zone, existing);
                return Ok(store_ok(false));
            }
        }
    };
    if value.stored_len() > MAX_VALUE_BYTES {
        return Ok(store_err("value too long"));
    }
    let expire_ms = match expire_from_exptime(exptime, now_ms()) {
        Ok(ms) => ms,
        Err(err) => return Ok(store_err(err)),
    };
    match mode {
        StoreMode::Add | StoreMode::SafeAdd => {
            if live.is_some() {
                return Ok(store_err("exists"));
            }
        }
        StoreMode::Replace => {
            if live.is_none() {
                return Ok(store_err("not found"));
            }
        }
        StoreMode::Set | StoreMode::SafeSet => {}
    }
    let safe = matches!(mode, StoreMode::SafeSet | StoreMode::SafeAdd);
    if let Some(node) = live.or(if existing.is_null() {
        None
    } else {
        Some(existing)
    }) {
        let sd = node_sd(node);
        if !is_list(sd) && unsafe { (*sd).value_len as usize } == value.stored_len() {
            inplace_update(sd, &value, flags, expire_ms);
            touch_lru(sh, sd);
            return Ok(store_ok(false));
        }
        match alloc_replace(&mut zone, key, &value, flags, expire_ms, safe, node) {
            Ok(forcible) => Ok(store_ok(forcible)),
            Err(err) => Ok(err),
        }
    } else {
        match alloc_replace(
            &mut zone,
            key,
            &value,
            flags,
            expire_ms,
            safe,
            ptr::null_mut(),
        ) {
            Ok(forcible) => Ok(store_ok(forcible)),
            Err(err) => Ok(err),
        }
    }
}

pub(super) fn delete(name: &str, key: &[u8]) -> Result<bool, &'static str> {
    if validate_key(key).is_err() {
        return Ok(false);
    }
    let mut zone = LockedZone::open(name)?;
    let node = lookup_node(zone.sh(), key);
    if node.is_null() {
        return Ok(false);
    }
    delete_node(&mut zone, node);
    Ok(true)
}

pub(super) fn incr(
    name: &str,
    key: &[u8],
    delta: DictValue,
    init: Option<DictValue>,
    init_ttl: f64,
) -> Result<StoreResult, &'static str> {
    if let Err(err) = validate_key(key) {
        return Ok(store_err(err));
    }
    let Some(delta) = delta.as_number() else {
        return Ok(store_err("not a number"));
    };
    let mut zone = LockedZone::open(name)?;
    let _ = expire_locked(&mut zone, 1, ptr::null_mut());
    let sh = zone.sh();
    let now = now_ms();
    let node = lookup_node(sh, key);
    if !node.is_null() && !expired(node_sd(node), now) {
        let sd = node_sd(node);
        if is_list(sd) {
            return Ok(store_err("not a number"));
        }
        let Some(current) = decode_value(sd).and_then(|v| v.as_number()) else {
            return Ok(store_err("not a number"));
        };
        let next = add_numbers(&current, &delta);
        if unsafe { (*sd).value_len as usize } == next.stored_len() {
            inplace_update(sd, &next, unsafe { (*sd).user_flags }, unsafe {
                (*sd).expires
            });
            touch_lru(sh, sd);
            return Ok(StoreResult {
                ok: true,
                error: None,
                forcible: false,
                value: Some(next),
            });
        }
        let flags = unsafe { (*sd).user_flags };
        let expire_ms = unsafe { (*sd).expires };
        match alloc_replace(&mut zone, key, &next, flags, expire_ms, false, node) {
            Ok(forcible) => Ok(StoreResult {
                ok: true,
                error: None,
                forcible,
                value: Some(next),
            }),
            Err(err) => Ok(err),
        }
    } else {
        let Some(init) = init.and_then(|value| value.as_number()) else {
            return Ok(store_err("not found"));
        };
        let next = add_numbers(&init, &delta);
        let expire_ms = match expire_from_exptime(init_ttl, now) {
            Ok(ms) => ms,
            Err(err) => return Ok(store_err(err)),
        };
        match alloc_replace(&mut zone, key, &next, 0, expire_ms, false, node) {
            Ok(forcible) => Ok(StoreResult {
                ok: true,
                error: None,
                forcible,
                value: Some(next),
            }),
            Err(err) => Ok(err),
        }
    }
}

pub(super) fn ttl(name: &str, key: &[u8]) -> Result<Result<f64, &'static str>, &'static str> {
    if let Err(err) = validate_key(key) {
        return Ok(Err(err));
    }
    let zone = LockedZone::open(name)?;
    let now = now_ms();
    let node = lookup_node(zone.sh(), key);
    if node.is_null() || expired(node_sd(node), now) {
        return Ok(Err("not found"));
    }
    let expires = unsafe { (*node_sd(node)).expires };
    if expires == 0 {
        Ok(Ok(0.0))
    } else {
        Ok(Ok((expires.saturating_sub(now) as f64) / 1000.0))
    }
}

pub(super) fn expire(name: &str, key: &[u8], exptime: f64) -> Result<StoreResult, &'static str> {
    if let Err(err) = validate_key(key) {
        return Ok(store_err(err));
    }
    let zone = LockedZone::open(name)?;
    let now = now_ms();
    let node = lookup_node(zone.sh(), key);
    if node.is_null() || expired(node_sd(node), now) {
        return Ok(store_err("not found"));
    }
    match expire_from_exptime(exptime, now) {
        Ok(ms) => {
            unsafe { (*node_sd(node)).expires = ms };
            Ok(store_ok(false))
        }
        Err(err) => Ok(store_err(err)),
    }
}

pub(super) fn capacity(name: &str) -> Result<usize, &'static str> {
    let zone = LockedZone::open(name)?;
    Ok(zone.capacity())
}

pub(super) fn free_space(name: &str) -> Result<usize, &'static str> {
    let zone = LockedZone::open(name)?;
    Ok(zone.pfree().saturating_mul(unsafe { ngx_pagesize }))
}

fn init_list_node(zone: &mut LockedZone, node: *mut ngx_rbtree_node_t, size: usize, key: &[u8]) {
    unsafe {
        ptr::write_bytes(node.cast::<u8>(), 0, size);
        let sd = node_sd(node);
        (*sd).value_type = T_LIST;
        (*sd).key_len = key.len() as u16;
        (*sd).value_len = 0;
        (*sd).expires = 0;
        (*sd).user_flags = 0;
        let data = ptr::addr_of_mut!((*sd).data).cast::<u8>();
        ptr::copy_nonoverlapping(key.as_ptr(), data, key.len());
        ngx_queue_init(list_queue(sd));
        (*node).key = ngx_crc32_short(key) as ngx_rbtree_key_t;
        let sh = zone.sh();
        ngx_rbtree_insert(ptr::addr_of_mut!((*sh).rbtree), node);
        ngx_queue_insert_after(
            ptr::addr_of_mut!((*sh).lru_queue),
            ptr::addr_of_mut!((*sd).queue),
        );
    }
}

fn push_list_item(
    zone: &mut LockedZone,
    sd: *mut ShdictNode,
    value: &DictValue,
    left: bool,
) -> Result<i64, StoreResult> {
    let len = unsafe { (*sd).value_len as usize };
    if len >= MAX_LIST_LEN {
        return Err(store_err("list too long"));
    }
    let n = list_item_alloc_size(value.stored_len());
    let ptr = zone.alloc(n);
    if ptr.is_null() {
        return Err(store_err("no memory"));
    }
    unsafe {
        ptr::write_bytes(ptr, 0, n);
        let lnode = ptr.cast::<ShdictListNode>();
        (*lnode).value_len = value.stored_len() as u32;
        (*lnode).value_type = value_type(value);
        encode_value(
            value,
            slice::from_raw_parts_mut(
                ptr::addr_of_mut!((*lnode).data).cast::<u8>(),
                value.stored_len(),
            ),
        );
        let queue = list_queue(sd);
        if left {
            ngx_queue_insert_after(queue, ptr::addr_of_mut!((*lnode).queue));
        } else {
            ngx_queue_insert_before(queue, ptr::addr_of_mut!((*lnode).queue));
        }
        (*sd).value_len = (len + 1) as u32;
        Ok((*sd).value_len as i64)
    }
}

pub(super) fn list_push(
    name: &str,
    key: &[u8],
    value: DictValue,
    left: bool,
) -> Result<StoreResult, &'static str> {
    if let Err(err) = validate_key(key) {
        return Ok(store_err(err));
    }
    if let Err(err) = validate_list_item(&value) {
        return Ok(store_err(err));
    }
    let mut zone = LockedZone::open(name)?;
    let _ = expire_locked(&mut zone, 1, ptr::null_mut());
    let sh = zone.sh();
    let existing = lookup_node(sh, key);
    let sd = if existing.is_null() {
        ptr::null_mut()
    } else {
        node_sd(existing)
    };
    let now = now_ms();
    let sd = if !sd.is_null() && expired(sd, now) {
        if !is_list(sd) {
            delete_node(&mut zone, existing);
            ptr::null_mut()
        } else {
            free_list_nodes(&mut zone, sd);
            unsafe {
                (*sd).expires = 0;
                (*sd).user_flags = 0;
            }
            touch_lru(zone.sh(), sd);
            sd
        }
    } else if !sd.is_null() {
        if !is_list(sd) {
            return Ok(store_err("value not a list"));
        }
        touch_lru(sh, sd);
        sd
    } else {
        ptr::null_mut()
    };
    let sd = if sd.is_null() {
        let size = list_entry_size(key);
        let node = zone.alloc(size).cast::<ngx_rbtree_node_t>();
        if node.is_null() {
            return Ok(store_err("no memory"));
        }
        init_list_node(&mut zone, node, size, key);
        node_sd(node)
    } else {
        sd
    };
    match push_list_item(&mut zone, sd, &value, left) {
        Ok(len) => Ok(StoreResult {
            ok: true,
            error: None,
            forcible: false,
            value: Some(DictValue::Int(len)),
        }),
        Err(err) => {
            if unsafe { (*sd).value_len } == 0 {
                delete_node(&mut zone, sd_rbtree_node(sd));
            }
            Ok(err)
        }
    }
}

pub(super) fn list_pop(name: &str, key: &[u8], left: bool) -> Result<GetResult, &'static str> {
    if let Err(err) = validate_key(key) {
        return Ok(empty_get(err));
    }
    let mut zone = LockedZone::open(name)?;
    let _ = expire_locked(&mut zone, 1, ptr::null_mut());
    let sh = zone.sh();
    let node = lookup_node(sh, key);
    if node.is_null() || expired(node_sd(node), now_ms()) {
        return Ok(GetResult {
            value: None,
            flags: 0,
            stale: false,
            error: None,
        });
    }
    let sd = node_sd(node);
    if !is_list(sd) {
        return Ok(GetResult {
            value: None,
            flags: 0,
            stale: false,
            error: Some("value not a list"),
        });
    }
    let queue = list_queue(sd);
    let item_q = unsafe { if left { (*queue).next } else { (*queue).prev } };
    if item_q == queue {
        delete_node(&mut zone, node);
        return Ok(GetResult {
            value: None,
            flags: 0,
            stale: false,
            error: None,
        });
    }
    let lnode = item_q.cast::<ShdictListNode>();
    let Some(value) = decode_list_item(lnode) else {
        return Ok(empty_get("bad value type"));
    };
    unsafe { ngx_queue_remove(item_q) };
    zone.free(item_q.cast());
    let remaining = unsafe { (*sd).value_len.saturating_sub(1) };
    if remaining == 0 {
        unsafe { (*sd).value_len = 0 };
        delete_node(&mut zone, node);
    } else {
        unsafe { (*sd).value_len = remaining };
        touch_lru(sh, sd);
    }
    Ok(GetResult {
        value: Some(value),
        flags: 0,
        stale: false,
        error: None,
    })
}

pub(super) fn llen(name: &str, key: &[u8]) -> Result<GetResult, &'static str> {
    if let Err(err) = validate_key(key) {
        return Ok(empty_get(err));
    }
    let mut zone = LockedZone::open(name)?;
    let _ = expire_locked(&mut zone, 1, ptr::null_mut());
    let sh = zone.sh();
    let node = lookup_node(sh, key);
    if node.is_null() || expired(node_sd(node), now_ms()) {
        return Ok(GetResult {
            value: Some(DictValue::Int(0)),
            flags: 0,
            stale: false,
            error: None,
        });
    }
    let sd = node_sd(node);
    if !is_list(sd) {
        return Ok(GetResult {
            value: None,
            flags: 0,
            stale: false,
            error: Some("value not a list"),
        });
    }
    touch_lru(sh, sd);
    Ok(GetResult {
        value: Some(DictValue::Int(unsafe { (*sd).value_len as i64 })),
        flags: 0,
        stale: false,
        error: None,
    })
}

pub(super) fn flush_all(name: &str) -> Result<(), &'static str> {
    let mut zone = LockedZone::open(name)?;
    let now = now_ms();
    let _ = flush_expired_locked(&mut zone, MAX_FLUSH_BATCH, MAX_FLUSH_BATCH, now);
    unsafe {
        let sh = zone.sh();
        let head = ptr::addr_of_mut!((*sh).lru_queue);
        let mut q = (*head).prev;
        let mut visits = 0usize;
        while q != head && visits < MAX_FLUSH_BATCH {
            visits += 1;
            let sd = q
                .byte_sub(offset_of!(ShdictNode, queue))
                .cast::<ShdictNode>();
            if !expired(sd, now) {
                (*sd).expires = 1;
            }
            q = (*q).prev;
        }
    }
    let _ = expire_locked(&mut zone, 0, ptr::null_mut());
    Ok(())
}

fn flush_expired_locked(
    zone: &mut LockedZone,
    result_limit: usize,
    visit_limit: usize,
    now: u64,
) -> i64 {
    let sh = zone.sh();
    let mut freed = 0i64;
    let mut visits = 0usize;
    unsafe {
        let head = ptr::addr_of_mut!((*sh).lru_queue);
        let mut q = (*head).prev;
        while q != head && visits < visit_limit && (freed as usize) < result_limit {
            visits += 1;
            let prev = (*q).prev;
            let sd = q
                .byte_sub(offset_of!(ShdictNode, queue))
                .cast::<ShdictNode>();
            if expired(sd, now) {
                let node = sd_rbtree_node(sd);
                free_list_nodes(zone, sd);
                ngx_queue_remove(q);
                ngx_rbtree_delete(ptr::addr_of_mut!((*sh).rbtree), node);
                zone.free(node.cast());
                freed += 1;
            }
            q = prev;
        }
    }
    freed
}

pub(super) fn flush_expired(name: &str, max: Option<i64>) -> Result<i64, &'static str> {
    let mut zone = LockedZone::open(name)?;
    let limit = enumeration_limit(max, MAX_FLUSH_BATCH);
    Ok(flush_expired_locked(
        &mut zone,
        limit,
        MAX_FLUSH_BATCH,
        now_ms(),
    ))
}

pub(super) fn get_keys(name: &str, max: Option<i64>) -> Result<Vec<Vec<u8>>, &'static str> {
    let zone = LockedZone::open(name)?;
    let limit = enumeration_limit(max, MAX_GET_KEYS);
    let sh = zone.sh();
    let now = now_ms();
    let mut keys = Vec::new();
    let mut visits = 0usize;
    unsafe {
        let head = ptr::addr_of_mut!((*sh).lru_queue);
        let mut q = (*head).prev;
        while q != head && keys.len() < limit && visits < MAX_GET_KEYS {
            visits += 1;
            let sd = q
                .byte_sub(offset_of!(ShdictNode, queue))
                .cast::<ShdictNode>();
            if !expired(sd, now) {
                keys.push(sd_key(sd).to_vec());
            }
            q = (*q).prev;
        }
    }
    Ok(keys)
}

/// Bind `rss_shared_dict` to `zone.data` + `zone.init` (OpenResty directive).
pub(crate) fn attach_shm_zone(
    cf: *mut ngx_conf_t,
    zone: &mut ngx_shm_zone_t,
    name: ngx_str_t,
) -> Result<(), ()> {
    // SAFETY: NGINX conf pool outlives the cycle's shm zone descriptors.
    let ctx =
        unsafe { ngx_pcalloc((*cf).pool, mem::size_of::<ShdictZoneCtx>()) }.cast::<ShdictZoneCtx>();
    if ctx.is_null() {
        return Err(());
    }
    unsafe {
        (*ctx).name = name;
        zone.init = Some(zone_init);
        zone.data = ctx.cast();
    }
    Ok(())
}

/// Zone init callback: NGINX `init(shm_zone, old_data)` reload contract.
pub unsafe extern "C" fn zone_init(
    shm_zone: *mut ngx_shm_zone_t,
    data: *mut core::ffi::c_void,
) -> ngx_int_t {
    let Some(zone) = (unsafe { shm_zone.as_mut() }) else {
        return Status::NGX_ERROR.into();
    };
    let ctx = zone.data.cast::<ShdictZoneCtx>();
    if ctx.is_null() {
        return Status::NGX_ERROR.into();
    }
    if !data.is_null() {
        // SAFETY: `data` is the previous cycle's ctx for this zone name+tag.
        let octx = data.cast::<ShdictZoneCtx>();
        unsafe {
            (*ctx).sh = (*octx).sh;
            (*ctx).shpool = (*octx).shpool;
        }
        return Status::NGX_OK.into();
    }
    let Some(shpool) = NonNull::new(zone.shm.addr.cast::<ngx_slab_pool_t>()) else {
        return Status::NGX_ERROR.into();
    };
    unsafe { (*ctx).shpool = shpool.as_ptr() };
    if !unsafe { shpool.as_ref().data.is_null() } {
        unsafe { (*ctx).sh = shpool.as_ref().data.cast() };
        return Status::NGX_OK.into();
    }
    unsafe { ngx_shmtx_lock(ptr::addr_of_mut!((*shpool.as_ptr()).mutex)) };
    let sh = unsafe { ngx_slab_alloc_locked(shpool.as_ptr(), mem::size_of::<ShdictShctx>()) }
        .cast::<ShdictShctx>();
    if sh.is_null() {
        unsafe { ngx_shmtx_unlock(ptr::addr_of_mut!((*shpool.as_ptr()).mutex)) };
        return Status::NGX_ERROR.into();
    }
    unsafe {
        ptr::write_bytes(sh.cast::<u8>(), 0, mem::size_of::<ShdictShctx>());
        ngx_rbtree_init(
            ptr::addr_of_mut!((*sh).rbtree),
            ptr::addr_of_mut!((*sh).sentinel),
            Some(shdict_rbtree_insert_value),
        );
        ngx_queue_init(ptr::addr_of_mut!((*sh).lru_queue));
        (*shpool.as_ptr()).data = sh.cast();
        (*shpool.as_ptr()).set_log_nomem(0);
        (*ctx).sh = sh;
        ngx_shmtx_unlock(ptr::addr_of_mut!((*shpool.as_ptr()).mutex));
    }
    Status::NGX_OK.into()
}
