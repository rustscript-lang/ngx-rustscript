//! NGINX rbtree key ordering and OpenResty shdict collision walk.
//!
//! `ngx_memn2cmp` and the equal-hash branch match
//! `nginx/src/core/ngx_string.c` / `ngx_http_lua_shdict_rbtree_insert_value`.

use std::cmp::Ordering;

/// 16-slot table from `nginx/src/core/ngx_crc32.c` (`ngx_crc32_table16`).
const NGX_CRC32_TABLE16: [u32; 16] = [
    0x0000_0000,
    0x1db7_1064,
    0x3b6e_20c8,
    0x26d9_30ac,
    0x76dc_4190,
    0x6b6b_51f4,
    0x4db2_6158,
    0x5005_713c,
    0xedb8_8320,
    0xf00f_9344,
    0xd6d6_a3e8,
    0xcb61_b38c,
    0x9b64_c2b0,
    0x86d3_d2d4,
    0xa00a_e278,
    0xbdbd_f21c,
];

/// `ngx_crc32_short` from `nginx/src/core/ngx_crc32.h`.
pub fn ngx_crc32_short(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc = NGX_CRC32_TABLE16[((crc ^ (u32::from(byte) & 0xf)) & 0xf) as usize] ^ (crc >> 4);
        crc = NGX_CRC32_TABLE16[((crc ^ (u32::from(byte) >> 4)) & 0xf) as usize] ^ (crc >> 4);
    }
    crc ^ 0xffff_ffff
}

/// `ngx_memn2cmp` from `nginx/src/core/ngx_string.c`.
pub fn nginx_memn2cmp(s1: &[u8], s2: &[u8]) -> i32 {
    let n = s1.len().min(s2.len());
    for i in 0..n {
        let d = i32::from(s1[i]) - i32::from(s2[i]);
        if d != 0 {
            return d;
        }
    }
    match s1.len().cmp(&s2.len()) {
        Ordering::Equal => 0,
        Ordering::Less => -1,
        Ordering::Greater => 1,
    }
}

#[derive(Clone, Debug)]
struct IndexNode {
    hash: u32,
    key: Vec<u8>,
    value: u32,
    left: Option<usize>,
    right: Option<usize>,
}

/// Testable BST using the OpenResty shdict insert/lookup comparison.
#[derive(Clone, Debug, Default)]
pub struct DictIndex {
    nodes: Vec<IndexNode>,
    root: Option<usize>,
}

impl DictIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// OpenResty `ngx_http_lua_shdict_rbtree_insert_value` without rebalance.
    pub fn insert(&mut self, hash: u32, key: &[u8], value: u32) {
        let idx = self.nodes.len();
        self.nodes.push(IndexNode {
            hash,
            key: key.to_vec(),
            value,
            left: None,
            right: None,
        });
        let Some(mut cur) = self.root else {
            self.root = Some(idx);
            return;
        };
        loop {
            let go_left = if hash < self.nodes[cur].hash {
                true
            } else if hash > self.nodes[cur].hash {
                false
            } else {
                nginx_memn2cmp(key, &self.nodes[cur].key) < 0
            };
            let next = if go_left {
                self.nodes[cur].left
            } else {
                self.nodes[cur].right
            };
            match next {
                Some(next) => cur = next,
                None => {
                    if go_left {
                        self.nodes[cur].left = Some(idx);
                    } else {
                        self.nodes[cur].right = Some(idx);
                    }
                    return;
                }
            }
        }
    }

    /// OpenResty `ngx_http_lua_shdict_lookup` key walk.
    pub fn lookup(&self, hash: u32, key: &[u8]) -> Option<u32> {
        let mut cur = self.root;
        while let Some(idx) = cur {
            let node = &self.nodes[idx];
            if hash < node.hash {
                cur = node.left;
            } else if hash > node.hash {
                cur = node.right;
            } else {
                let rc = nginx_memn2cmp(key, &node.key);
                if rc == 0 {
                    return Some(node.value);
                }
                cur = if rc < 0 { node.left } else { node.right };
            }
        }
        None
    }

    pub fn inorder_keys(&self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        self.walk(self.root, &mut out);
        out
    }

    fn walk(&self, idx: Option<usize>, out: &mut Vec<Vec<u8>>) {
        let Some(idx) = idx else {
            return;
        };
        self.walk(self.nodes[idx].left, out);
        out.push(self.nodes[idx].key.clone());
        self.walk(self.nodes[idx].right, out);
    }
}
