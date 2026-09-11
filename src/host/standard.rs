//! pd-vm standard async hosts for request-owned VMs.

use vm::{
    HttpConfig, HttpHostExt, IoHostExt, IoPolicy, SqliteHostExt, SqlitePolicy, Vm,
    standard_composition,
};

/// NGINX-facing grants for pd-vm I/O, HTTP, and SQLite.
///
/// Empty roots/allowlists deny external access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StandardHostPolicy {
    pub io_roots: Vec<String>,
    pub io_write: bool,
    pub io_max_read: usize,
    pub io_max_write: usize,
    pub http_hosts: Vec<String>,
    pub http_schemes: Vec<String>,
    pub http_ports: Vec<u16>,
    pub http_allow_private: bool,
    pub sqlite_root: Option<String>,
}

impl StandardHostPolicy {
    pub fn fail_closed() -> Self {
        Self {
            io_roots: Vec::new(),
            io_write: false,
            io_max_read: 1024 * 1024,
            io_max_write: 1024 * 1024,
            http_hosts: Vec::new(),
            http_schemes: Vec::new(),
            http_ports: Vec::new(),
            http_allow_private: false,
            sqlite_root: None,
        }
    }
}

impl Default for StandardHostPolicy {
    fn default() -> Self {
        Self::fail_closed()
    }
}

/// Install pd-vm's standard runtime composition and embedding policies.
pub fn install_standard_runtime(vm: &mut Vm, policy: &StandardHostPolicy) -> Result<(), String> {
    let composition = standard_composition();
    vm.set_standard_composition(composition.clone());
    vm.configure_io(IoPolicy {
        allowed_roots: policy.io_roots.clone(),
        allow_write: policy.io_write,
        allow_process: false,
        max_read_bytes: policy.io_max_read,
        max_write_bytes: policy.io_max_write,
    });
    vm.configure_http(HttpConfig {
        allowed_schemes: policy.http_schemes.clone(),
        allowed_hosts: policy.http_hosts.clone(),
        allowed_ports: policy.http_ports.clone(),
        allow_private_ips: policy.http_allow_private,
        ..HttpConfig::default()
    })
    .map_err(|err| err.to_string())?;
    vm.configure_sqlite(SqlitePolicy {
        database_root: policy.sqlite_root.clone(),
        allow_unsafe_sql: false,
        ..SqlitePolicy::default()
    });
    let standard_imports: Vec<String> = vm
        .program()
        .imports
        .iter()
        .filter(|import| composition.import_in_standard(import))
        .map(|import| import.name.clone())
        .collect();
    for name in standard_imports {
        if !composition.bind_default_name(vm, &name) {
            return Err(format!("cannot bind standard host import '{name}'"));
        }
    }
    Ok(())
}
