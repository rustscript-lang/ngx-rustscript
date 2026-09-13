//! Task 11 request-body replacement and builder hosts.

use ngx_rustscript_nginx_test_stubs as nginx_stubs;

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::mem::MaybeUninit;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nginx_sys::{ngx_http_request_body_t, ngx_http_request_t};
use ngx_rustscript::config::{
    ConfigError, compile_rss_source, compile_rss_source_for_phase, program_requires_request_state,
    validate_program_imports_for_phase,
};
use ngx_rustscript::host::{
    AllowedOps, HostCapability, HostFamily, HostSyncKind, NgxPhase, bind_ngx_hosts,
    host_allowed_in_phase, manifest_entry, ngx_host_catalog, ngx_host_manifest,
    production_runtime_host_names, with_request_scope,
};
use ngx_rustscript::notify::RequestIdentity;
use vm::{HostTypeSchema, Program, Value, Vm, VmMap, VmStatus};

mod support;
use support::artifact::module_cdylib;
use support::nginx::{
    combined, nginx_binary, nginx_process_group_command, nginx_quote_path, nginx_test,
    terminate_process_group,
};
use support::temp::isolated_prefix;

const SET_BODY_DATA: &str = "ngx::req::set_body_data";
const SET_BODY_FILE: &str = "ngx::req::set_body_file";
const INIT_BODY: &str = "ngx::req::init_body";
const APPEND_BODY: &str = "ngx::req::append_body";
const FINISH_BODY: &str = "ngx::req::finish_body";

const MUTATION_HOSTS: &[&str] = &[
    SET_BODY_DATA,
    SET_BODY_FILE,
    INIT_BODY,
    APPEND_BODY,
    FINISH_BODY,
];

fn compile(source: &str) -> Arc<Program> {
    compile_rss_source(source).unwrap_or_else(|err| panic!("compile failed: {err}\n{source}"))
}

fn alloc_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn dummy_request() -> Box<ngx_http_request_t> {
    let mut req = Box::new(unsafe { MaybeUninit::<ngx_http_request_t>::zeroed().assume_init() });
    let ptr = (&mut *req) as *mut ngx_http_request_t;
    req.main = ptr;
    // ngx_palloc stub ignores the pool pointer; install only requires non-null.
    req.pool = std::ptr::dangling_mut();
    req
}

fn eval_in_request_ops(
    request: *mut ngx_http_request_t,
    ops: AllowedOps,
    phase: NgxPhase,
    source: &str,
) -> Result<Value, String> {
    let program = compile(source);
    let mut vm = Vm::try_new((*program).clone()).expect("construct vm");
    bind_ngx_hosts(&mut vm);
    with_request_scope(
        request,
        RequestIdentity::new(1, 1),
        phase,
        ops,
        || match vm.run() {
            Ok(VmStatus::Halted) => Ok(vm
                .stack()
                .last()
                .cloned()
                .unwrap_or_else(|| panic!("expected a stack value for {source}"))),
            Ok(status) => Err(format!("unexpected status {status:?}")),
            Err(err) => Err(err.to_string()),
        },
    )
}

fn eval_in_request(request: *mut ngx_http_request_t, source: &str) -> Result<Value, String> {
    eval_in_request_ops(
        request,
        AllowedOps::for_phase(NgxPhase::Access),
        NgxPhase::Access,
        source,
    )
}

fn expect_string(value: Value) -> String {
    match value {
        Value::String(text) => (*text).clone(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn expect_map(value: &Value) -> &VmMap {
    match value {
        Value::Map(map) => map,
        other => panic!("expected map, got {other:?}"),
    }
}

fn map_get<'a>(map: &'a VmMap, key: &str) -> &'a Value {
    map.get(&Value::string(key))
        .unwrap_or_else(|| panic!("missing map key {key}"))
}

fn schema(name: &str) -> vm::HostFunctionSchema {
    ngx_host_catalog()
        .function(name)
        .cloned()
        .unwrap_or_else(|| panic!("missing catalog {name}"))
}

fn param_names(name: &str) -> Vec<String> {
    schema(name)
        .params
        .iter()
        .map(|param| param.name.clone())
        .collect()
}

fn param_types(name: &str) -> Vec<HostTypeSchema> {
    schema(name)
        .params
        .iter()
        .map(|param| param.ty.clone())
        .collect()
}

#[allow(clippy::vec_box)]
struct LiveBody {
    _body: Box<ngx_http_request_body_t>,
    _bufs: Vec<Box<nginx_sys::ngx_buf_t>>,
    _chains: Vec<Box<nginx_sys::ngx_chain_t>>,
}

fn install_in_memory_body(req: &mut ngx_http_request_t, chunks: &mut [Vec<u8>]) -> LiveBody {
    let mut body =
        Box::new(unsafe { MaybeUninit::<ngx_http_request_body_t>::zeroed().assume_init() });
    let mut bufs = Vec::new();
    let mut chains = Vec::new();
    for chunk in chunks.iter_mut() {
        let mut buf =
            Box::new(unsafe { MaybeUninit::<nginx_sys::ngx_buf_t>::zeroed().assume_init() });
        buf.pos = chunk.as_mut_ptr();
        buf.last = unsafe { chunk.as_mut_ptr().add(chunk.len()) };
        buf.start = chunk.as_mut_ptr();
        buf.end = buf.last;
        bufs.push(buf);
        chains.push(Box::new(unsafe {
            MaybeUninit::<nginx_sys::ngx_chain_t>::zeroed().assume_init()
        }));
    }
    for i in 0..chains.len() {
        chains[i].buf = bufs[i].as_mut() as *mut _;
        chains[i].next = if i + 1 < chains.len() {
            chains[i + 1].as_mut() as *mut _
        } else {
            std::ptr::null_mut()
        };
    }
    if !chains.is_empty() {
        body.bufs = chains[0].as_mut() as *mut _;
        body.buf = bufs[0].as_mut() as *mut _;
    }
    req.request_body = body.as_mut() as *mut _;
    LiveBody {
        _body: body,
        _bufs: bufs,
        _chains: chains,
    }
}

#[test]
fn production_host_count_is_195_with_body_mutation_hosts() {
    assert_eq!(production_runtime_host_names().len(), 195);
    assert_eq!(ngx_host_catalog().functions().len(), 195);
    assert_eq!(ngx_host_manifest().len(), 195);
    let runtime: std::collections::BTreeSet<&str> =
        production_runtime_host_names().iter().copied().collect();
    for name in MUTATION_HOSTS {
        assert!(runtime.contains(name), "missing {name}");
    }
}

#[test]
fn body_mutation_catalog_schemas_phases_and_capabilities() {
    for name in MUTATION_HOSTS {
        let entry = manifest_entry(name).unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(entry.family, HostFamily::Request);
        assert_eq!(entry.sync, HostSyncKind::Sync);
        assert!(entry.capabilities.contains(&HostCapability::RequestWrite));
        assert!(!entry.capabilities.contains(&HostCapability::RequestRead));
        assert_eq!(
            entry.phases,
            &[NgxPhase::Rewrite, NgxPhase::Access, NgxPhase::Content]
        );
        assert!(host_allowed_in_phase(name, NgxPhase::Access));
        assert!(host_allowed_in_phase(name, NgxPhase::Content));
        assert!(host_allowed_in_phase(name, NgxPhase::Rewrite));
        assert!(!host_allowed_in_phase(name, NgxPhase::Log));
        assert!(!host_allowed_in_phase(name, NgxPhase::InitWorker));
        assert!(program_requires_request_state(&compile(&format!(
            "use ngx;\n{name}{};\n",
            match *name {
                SET_BODY_DATA => "(\"x\")",
                SET_BODY_FILE => "(\"/tmp/x\", null)",
                INIT_BODY => "(null)",
                APPEND_BODY => "(\"x\")",
                FINISH_BODY => "()",
                _ => unreachable!(),
            }
        ))));
    }

    assert_eq!(param_names(SET_BODY_DATA), ["data"]);
    assert_eq!(param_types(SET_BODY_DATA), [HostTypeSchema::String]);
    assert_eq!(schema(SET_BODY_DATA).return_type, HostTypeSchema::Null);

    assert_eq!(param_names(SET_BODY_FILE), ["path", "auto_clean"]);
    assert_eq!(
        param_types(SET_BODY_FILE),
        [HostTypeSchema::String, HostTypeSchema::Unknown]
    );
    assert_eq!(schema(SET_BODY_FILE).return_type, HostTypeSchema::Null);

    assert_eq!(param_names(INIT_BODY), ["buffer_size"]);
    assert_eq!(param_types(INIT_BODY), [HostTypeSchema::Unknown]);
    assert_eq!(schema(INIT_BODY).return_type, HostTypeSchema::Null);

    assert_eq!(param_names(APPEND_BODY), ["data"]);
    assert_eq!(param_types(APPEND_BODY), [HostTypeSchema::String]);
    assert_eq!(schema(APPEND_BODY).return_type, HostTypeSchema::Null);

    assert!(param_names(FINISH_BODY).is_empty());
    assert_eq!(schema(FINISH_BODY).return_type, HostTypeSchema::Null);
}

#[test]
fn body_mutation_rejected_in_log_and_without_live_request_or_write() {
    for source in [
        "use ngx;\nngx::req::set_body_data(\"x\");\n",
        "use ngx;\nngx::req::set_body_file(\"/tmp/x\", null);\n",
        "use ngx;\nngx::req::init_body(null);\n",
        "use ngx;\nngx::req::append_body(\"x\");\n",
        "use ngx;\nngx::req::finish_body();\n",
    ] {
        match compile_rss_source_for_phase(source, NgxPhase::Log) {
            Err(ConfigError::PhaseForbidden { .. }) | Err(ConfigError::UnsupportedImport(_)) => {}
            other => panic!("log compile must reject mutation, got {other:?} for {source}"),
        }
        validate_program_imports_for_phase(&compile(source), NgxPhase::Log)
            .expect_err("log import validation must reject mutation");
    }

    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let no_scope = {
        let program = compile("use ngx;\nngx::req::set_body_data(\"x\");\n");
        let mut vm = Vm::try_new((*program).clone()).expect("vm");
        bind_ngx_hosts(&mut vm);
        match vm.run() {
            Err(err) => err.to_string(),
            Ok(status) => panic!("expected host error without request, got {status:?}"),
        }
    };
    assert!(
        no_scope.to_ascii_lowercase().contains("request"),
        "missing request must fail, got {no_scope}"
    );

    let read_only = eval_in_request_ops(
        ptr,
        AllowedOps::REQUEST_READ,
        NgxPhase::Access,
        "use ngx;\nngx::req::set_body_data(\"x\");\n",
    )
    .expect_err("read-only");
    assert!(
        read_only.to_ascii_lowercase().contains("write")
            || read_only.to_ascii_lowercase().contains("capability")
            || read_only.to_ascii_lowercase().contains("allowed")
            || read_only.to_ascii_lowercase().contains("scope"),
        "RequestWrite required, got {read_only}"
    );
}

#[test]
fn body_mutation_rejects_header_sent_reading_discarded_and_subrequest() {
    nginx_stubs::force_link();
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let mut original = b"old-body".to_vec();
    let _live = install_in_memory_body(&mut req, std::slice::from_mut(&mut original));

    nginx_stubs::write_header_sent(ptr.cast(), true);
    let err = eval_in_request(ptr, "use ngx;\nngx::req::set_body_data(\"new\");\n")
        .expect_err("header sent");
    assert!(
        err.to_ascii_lowercase().contains("header"),
        "header_sent must fail, got {err}"
    );
    nginx_stubs::write_header_sent(ptr.cast(), false);

    nginx_stubs::write_reading_body(ptr.cast(), true);
    let err =
        eval_in_request(ptr, "use ngx;\nngx::req::set_body_data(\"new\");\n").expect_err("reading");
    assert!(
        err.to_ascii_lowercase().contains("read") || err.to_ascii_lowercase().contains("pending"),
        "reading_body must fail, got {err}"
    );
    nginx_stubs::write_reading_body(ptr.cast(), false);

    nginx_stubs::write_discard_body(ptr.cast(), true);
    let err = eval_in_request(ptr, "use ngx;\nngx::req::set_body_data(\"new\");\n")
        .expect_err("discarded");
    assert!(
        err.to_ascii_lowercase().contains("discard"),
        "discard_body must fail, got {err}"
    );
    nginx_stubs::write_discard_body(ptr.cast(), false);

    let mut child = dummy_request();
    child.main = ptr;
    let child_ptr = child.as_mut() as *mut ngx_http_request_t;
    let err = eval_in_request(child_ptr, "use ngx;\nngx::req::set_body_data(\"new\");\n")
        .expect_err("subrequest");
    assert!(
        err.to_ascii_lowercase().contains("subrequest"),
        "subrequest mutation must fail, got {err}"
    );
}

#[test]
fn set_body_data_replaces_memory_and_getters_see_it() {
    let _lock = alloc_guard();
    nginx_stubs::stub_alloc_reset();
    nginx_stubs::stub_alloc_fail_after(64);
    let mut req = dummy_request();
    req.pool = std::ptr::dangling_mut();
    let mut original = b"stale-original".to_vec();
    let _live = install_in_memory_body(&mut req, std::slice::from_mut(&mut original));
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let value = eval_in_request(
        ptr,
        r#"
use ngx;
ngx::req::set_body_data("alpha=1&beta=2");
ngx::req::get_body_data();
"#,
    )
    .expect("set_body_data");
    assert_eq!(expect_string(value), "alpha=1&beta=2");
    let post = eval_in_request(ptr, "use ngx;\nngx::req::get_post_args(null);\n").expect("post");
    let parsed = expect_map(map_get(expect_map(&post), "args"));
    assert_eq!(map_get(parsed, "alpha"), &Value::string("1"));
    assert_eq!(map_get(parsed, "beta"), &Value::string("2"));
    nginx_stubs::stub_alloc_reset();
}

#[test]
fn set_body_data_allocation_failure_leaves_prior_body() {
    let _lock = alloc_guard();
    nginx_stubs::stub_alloc_reset();
    nginx_stubs::stub_alloc_fail_after(0);
    let mut req = dummy_request();
    req.pool = std::ptr::dangling_mut();
    let mut original = b"keep-me".to_vec();
    let _live = install_in_memory_body(&mut req, std::slice::from_mut(&mut original));
    let prior = req.request_body;
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let err =
        eval_in_request(ptr, "use ngx;\nngx::req::set_body_data(\"nope\");\n").expect_err("oom");
    assert!(
        err.to_ascii_lowercase().contains("memory") || err.to_ascii_lowercase().contains("alloc"),
        "oom must mention memory, got {err}"
    );
    assert_eq!(req.request_body, prior, "prior body pointer must remain");
    nginx_stubs::stub_alloc_fail_after(64);
    let kept = eval_in_request(ptr, "use ngx;\nngx::req::get_body_data();\n").expect("kept");
    assert_eq!(expect_string(kept), "keep-me");
    nginx_stubs::stub_alloc_reset();
}

#[test]
fn builder_state_machine_init_append_finish_and_failures() {
    let _lock = alloc_guard();
    nginx_stubs::stub_alloc_reset();
    nginx_stubs::stub_alloc_fail_after(64);
    let mut req = dummy_request();
    req.pool = std::ptr::dangling_mut();
    let ptr = req.as_mut() as *mut ngx_http_request_t;

    let err = eval_in_request(ptr, "use ngx;\nngx::req::append_body(\"x\");\n")
        .expect_err("append without init");
    assert!(
        err.to_ascii_lowercase().contains("init"),
        "append without init, got {err}"
    );
    let err = eval_in_request(ptr, "use ngx;\nngx::req::finish_body();\n")
        .expect_err("finish without init");
    assert!(
        err.to_ascii_lowercase().contains("init"),
        "finish without init, got {err}"
    );

    let value = eval_in_request(
        ptr,
        r#"
use ngx;
ngx::req::init_body(null);
ngx::req::init_body(8);
ngx::req::append_body("ab");
ngx::req::append_body("cd");
ngx::req::finish_body();
ngx::req::get_body_data();
"#,
    )
    .expect("builder");
    assert_eq!(expect_string(value), "abcd");

    let err =
        eval_in_request(ptr, "use ngx;\nngx::req::finish_body();\n").expect_err("repeated finish");
    assert!(
        err.to_ascii_lowercase().contains("init") || err.to_ascii_lowercase().contains("finish"),
        "repeated finish, got {err}"
    );

    let empty = eval_in_request(
        ptr,
        r#"
use ngx;
ngx::req::init_body(4);
ngx::req::finish_body();
ngx::req::get_body_data();
"#,
    )
    .expect("empty");
    assert_eq!(expect_string(empty), "");

    let err = eval_in_request(
        ptr,
        r#"
use ngx;
ngx::req::init_body(2);
ngx::req::append_body("xyz");
"#,
    )
    .expect_err("oversize");
    assert!(
        err.to_ascii_lowercase().contains("cap")
            || err.to_ascii_lowercase().contains("limit")
            || err.to_ascii_lowercase().contains("size"),
        "oversize, got {err}"
    );

    let replaced = eval_in_request(
        ptr,
        r#"
use ngx;
ngx::req::init_body(null);
ngx::req::append_body("pending");
ngx::req::set_body_data("direct");
ngx::req::get_body_data();
"#,
    )
    .expect("set during builder");
    assert_eq!(expect_string(replaced), "direct");
    nginx_stubs::stub_alloc_reset();
}

#[test]
fn builder_finish_install_failure_restores_builder_and_retries() {
    let _lock = alloc_guard();
    nginx_stubs::stub_alloc_reset();
    nginx_stubs::stub_alloc_fail_after(64);
    let mut req = dummy_request();
    req.pool = std::ptr::dangling_mut();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    eval_in_request(
        ptr,
        r#"
use ngx;
ngx::req::init_body(null);
ngx::req::append_body("retry-me");
"#,
    )
    .expect("init append");

    nginx_stubs::stub_alloc_fail_after(0);
    let err = eval_in_request(ptr, "use ngx;\nngx::req::finish_body();\n").expect_err("finish oom");
    assert!(
        err.to_ascii_lowercase().contains("memory") || err.to_ascii_lowercase().contains("alloc"),
        "finish install failure must be allocation, got {err}"
    );
    assert!(
        !err.contains("requires ngx::req::init_body"),
        "failed finish must restore builder, got {err}"
    );

    nginx_stubs::stub_alloc_fail_after(64);
    let value = eval_in_request(
        ptr,
        r#"
use ngx;
ngx::req::finish_body();
ngx::req::get_body_data();
"#,
    )
    .expect("retry finish");
    assert_eq!(expect_string(value), "retry-me");
    nginx_stubs::stub_alloc_reset();
}

#[test]
fn set_body_file_rejects_directory_symlink_and_missing() {
    let _lock = alloc_guard();
    nginx_stubs::stub_alloc_reset();
    nginx_stubs::stub_alloc_fail_after(64);
    let mut req = dummy_request();
    req.pool = std::ptr::dangling_mut();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let dir = isolated_prefix("ngx-rss-body-file-reject");
    let dir_path = dir.path().to_string_lossy().replace('\\', "\\\\");
    let err = eval_in_request(
        ptr,
        &format!("use ngx;\nngx::req::set_body_file(\"{dir_path}\", false);\n"),
    )
    .expect_err("directory");
    assert!(
        err.to_ascii_lowercase().contains("regular")
            || err.to_ascii_lowercase().contains("directory")
            || err.to_ascii_lowercase().contains("file"),
        "directory reject, got {err}"
    );

    let missing = eval_in_request(
        ptr,
        "use ngx;\nngx::req::set_body_file(\"/no/such/ngx-rss-body\", false);\n",
    )
    .expect_err("missing");
    assert!(
        missing.to_ascii_lowercase().contains("open")
            || missing.to_ascii_lowercase().contains("file")
            || missing.to_ascii_lowercase().contains("not"),
        "missing file, got {missing}"
    );
    nginx_stubs::stub_alloc_reset();
}

#[test]
fn set_body_file_rejects_fifo_and_symlink() {
    let _lock = alloc_guard();
    nginx_stubs::stub_alloc_reset();
    nginx_stubs::stub_alloc_fail_after(64);
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let dir = isolated_prefix("ngx-rss-body-file-special");

    let fifo = dir.path().join("body.fifo");
    let fifo_c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let fifo_path = fifo.to_string_lossy().replace('\\', "\\\\");
    let err = eval_in_request(
        ptr,
        &format!("use ngx;\nngx::req::set_body_file(\"{fifo_path}\", false);\n"),
    )
    .expect_err("fifo");
    assert!(
        err.to_ascii_lowercase().contains("regular") || err.to_ascii_lowercase().contains("file"),
        "fifo reject, got {err}"
    );

    let src = dir.path().join("body.txt");
    fs::write(&src, b"ok").unwrap();
    let link = dir.path().join("body.link");
    std::os::unix::fs::symlink(&src, &link).unwrap();
    let link_path = link.to_string_lossy().replace('\\', "\\\\");
    let err = eval_in_request(
        ptr,
        &format!("use ngx;\nngx::req::set_body_file(\"{link_path}\", false);\n"),
    )
    .expect_err("symlink");
    assert!(
        err.to_ascii_lowercase().contains("symlink")
            || err.to_ascii_lowercase().contains("open")
            || err.to_ascii_lowercase().contains("file"),
        "symlink reject, got {err}"
    );
    nginx_stubs::stub_alloc_reset();
}

#[test]
fn set_body_file_empty_file_installs_file_backed_body() {
    let _lock = alloc_guard();
    nginx_stubs::stub_alloc_reset();
    nginx_stubs::stub_alloc_fail_after(64);
    let mut req = dummy_request();
    let ptr = req.as_mut() as *mut ngx_http_request_t;
    let dir = isolated_prefix("ngx-rss-body-file-empty");
    let path = dir.path().join("empty.txt");
    fs::write(&path, b"").unwrap();
    let lstat = fs::symlink_metadata(&path).unwrap();
    let path_s = path.to_string_lossy().replace('\\', "\\\\");
    let got = expect_string(
        eval_in_request(
            ptr,
            &format!(
                "use ngx;\nngx::req::set_body_file(\"{path_s}\", false);\nngx::req::get_body_file();\n"
            ),
        )
        .expect("empty file"),
    );
    assert!(
        got.contains("empty.txt"),
        "empty file body should keep the path, got {got}"
    );
    let opened = fs::File::open(&path).unwrap();
    let fstat = opened.metadata().unwrap();
    assert_eq!(
        (lstat.dev(), lstat.ino()),
        (fstat.dev(), fstat.ino()),
        "set_body_file success path must keep matching lstat/fstat identity"
    );
    let data = eval_in_request(ptr, "use ngx;\nngx::req::get_body_data();\n").expect("data");
    assert!(
        matches!(data, Value::Null),
        "file-backed empty body get_body_data must be null, got {data:?}"
    );
    nginx_stubs::stub_alloc_reset();
}

struct RunningNginx {
    child: Child,
}

impl Drop for RunningNginx {
    fn drop(&mut self) {
        terminate_process_group(&mut self.child, Duration::from_secs(2));
    }
}

fn quit_and_wait(child: &mut Child, timeout: Duration) {
    let pid = child.id();
    let status = Command::new("kill")
        .args(["-QUIT", &pid.to_string()])
        .status()
        .expect("send SIGQUIT");
    assert!(status.success(), "kill -QUIT {pid} failed: {status:?}");
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                assert!(
                    status.success(),
                    "nginx SIGQUIT must exit successfully: {status:?}"
                );
                return;
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    panic!("nginx master pid {pid} did not exit after SIGQUIT within {timeout:?}");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(err) => panic!("wait nginx: {err}"),
        }
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn spawn_nginx(nginx: &Path, prefix: &Path) -> RunningNginx {
    let stderr = fs::File::create(prefix.join("logs/nginx.spawn.err")).unwrap();
    let child = nginx_process_group_command(nginx)
        .arg("-p")
        .arg(prefix)
        .arg("-c")
        .arg("conf/nginx.conf")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("spawn nginx");
    RunningNginx { child }
}

fn wait_for_port(port: u16, timeout: Duration, prefix: &Path) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "port {port} did not open; spawn.err:\n{}\nerror.log:\n{}",
        fs::read_to_string(prefix.join("logs/nginx.spawn.err")).unwrap_or_default(),
        fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default()
    );
}

fn http_exchange(port: u16, request: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let _ = stream.flush();
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
    let response = String::from_utf8_lossy(&buf).into_owned();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|tok| tok.parse().ok())
        .unwrap_or(0);
    (status, response)
}

fn parse_http_response(response: &str) -> (u16, String) {
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|tok| tok.parse().ok())
        .unwrap_or(0);
    let payload = response.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, payload)
}

fn wait_access_status(log: &Path, method: &str, uri: &str, status: u16, timeout: Duration) -> bool {
    let needle = format!("\"{method} {uri} ");
    let status_tok = format!(" {status} ");
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(text) = fs::read_to_string(log) {
            if text
                .lines()
                .any(|line| line.contains(&needle) && line.contains(&status_tok))
            {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn wait_log_contains(log: &Path, needle: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if fs::read_to_string(log).unwrap_or_default().contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn wait_path_absent(path: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn extract_rss_body_path(buf: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(buf);
    let needle = "ngx-rss-body-";
    let idx = text.find(needle)?;
    let start = text[..idx]
        .rfind(['\r', '\n', '"', ' ', ';', '='])
        .map(|i| i + 1)
        .unwrap_or(0);
    let path: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_graphic() && *c != '"' && *c != ';')
        .collect();
    if path.starts_with('/') && path.contains(needle) {
        Some(path)
    } else {
        None
    }
}

fn capture_abort_copy_path(port: u16, timeout: Duration) -> (TcpStream, String, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    stream
        .write_all(
            b"POST /abort/ HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\nold",
        )
        .unwrap();
    let _ = stream.flush();
    let start = Instant::now();
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    while start.elapsed() < timeout {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(path) = extract_rss_body_path(&buf) {
                    return (stream, path, buf);
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                if let Some(path) = extract_rss_body_path(&buf) {
                    return (stream, path, buf);
                }
            }
            Err(_) => break,
        }
    }
    panic!(
        "abort client did not capture private copy path within {timeout:?}: {}",
        String::from_utf8_lossy(&buf)
    );
}

fn write_script(path: &Path, source: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source.trim_start()).unwrap();
}

fn body_conf(module: &Path, prefix: &Path, port: u16, extra: &str) -> String {
    format!(
        r#"
worker_processes 1;
error_log "{error_log}" info;
pid {pid};
daemon off;
master_process on;
load_module {module};

events {{
    worker_connections 64;
}}

http {{
    access_log {access};
    default_type text/plain;
    client_body_timeout 2s;
    client_max_body_size 32k;
    server {{
        listen 127.0.0.1:{port} sndbuf=4k;
        server_name localhost;
        root {html};
        {extra}
    }}
}}
"#,
        error_log = prefix.join("logs/error.log").display(),
        pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap(),
        module = nginx_quote_path(module).unwrap(),
        access = nginx_quote_path(&prefix.join("logs/access.log")).unwrap(),
        html = nginx_quote_path(&prefix.join("html")).unwrap(),
    )
}
struct EchoCapture {
    method: String,
    request_target: String,
    content_length: Option<String>,
    content_length_count: usize,
    transfer_encoding: Option<String>,
    body: Vec<u8>,
}

fn start_echo_upstream() -> (
    u16,
    std::sync::mpsc::Receiver<EchoCapture>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(false).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    loop {
                        match stream.read(&mut tmp) {
                            Ok(0) => break,
                            Ok(n) => {
                                buf.extend_from_slice(&tmp[..n]);
                                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let header_end = buf
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map(|i| i + 4)
                        .unwrap_or(buf.len());
                    let header_text = String::from_utf8_lossy(&buf[..header_end]).into_owned();
                    let mut method = String::new();
                    let mut request_target = String::new();
                    if let Some(first) = header_text.lines().next() {
                        let mut parts = first.split_whitespace();
                        method = parts.next().unwrap_or("").to_string();
                        request_target = parts.next().unwrap_or("").to_string();
                    }
                    let mut content_length = None;
                    let mut content_length_count = 0usize;
                    let mut transfer_encoding = None;
                    let mut declared = 0usize;
                    for line in header_text.lines() {
                        let lower = line.to_ascii_lowercase();
                        if let Some(rest) = lower.strip_prefix("content-length:") {
                            let raw = line.split_once(':').map(|(_, v)| v.trim().to_string());
                            content_length = raw.clone();
                            content_length_count += 1;
                            declared = rest.trim().parse().unwrap_or(0);
                        }
                        if lower.starts_with("transfer-encoding:") {
                            transfer_encoding =
                                line.split_once(':').map(|(_, v)| v.trim().to_string());
                        }
                    }
                    let mut body = buf[header_end..].to_vec();
                    while body.len() < declared {
                        match stream.read(&mut tmp) {
                            Ok(0) => break,
                            Ok(n) => body.extend_from_slice(&tmp[..n]),
                            Err(_) => break,
                        }
                    }
                    if declared > 0 {
                        body.truncate(declared);
                    }
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.0 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    );
                    let _ = stream.write_all(&body);
                    let _ = tx.send(EchoCapture {
                        method,
                        request_target,
                        content_length,
                        content_length_count,
                        transfer_encoding,
                        body,
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    (port, rx, handle)
}

#[test]
fn nginx_body_mutation_upstream_subrequest_cleanup_and_isolation() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let port = free_port();
    let prefix = isolated_prefix("ngx-rustscript-req-body-mut");
    fs::create_dir_all(prefix.join("html")).unwrap();
    fs::write(prefix.join("html/index.html"), "fallback\n").unwrap();
    fs::write(
        prefix.join("html/ssi.html"),
        "SSI_CHILD=<!--# include virtual=\"/ssi-child\" -->\n",
    )
    .unwrap();

    let source_true = prefix.join("html/source-true.txt");
    fs::write(&source_true, b"file-true-payload").unwrap();
    let source_false = prefix.join("html/source-false.txt");
    fs::write(&source_false, b"file-false-payload").unwrap();
    let source_true_path = source_true.display().to_string();
    let source_false_path = source_false.display().to_string();

    let (echo_port, echo_rx, echo_thread) = start_echo_upstream();

    let mem = prefix.join("conf/mut_mem.rss");
    write_script(
        &mem,
        r#"
use ngx;
ngx::req::read_body();
ngx::req::set_body_data("replaced-memory");
ngx::log(ngx::log_level::notice(), ngx::encode_args({ mem_cl: ngx::var::get("content_length") }));
ngx::say(ngx::req::get_body_data());
"#,
    );
    let mem_proxy = prefix.join("conf/mut_mem_proxy.rss");
    write_script(
        &mem_proxy,
        r#"
use ngx;
ngx::req::read_body();
ngx::req::set_body_data("replaced-memory");
ngx::log(ngx::log_level::notice(), ngx::encode_args({ mem_proxy_cl: ngx::var::get("content_length") }));
"#,
    );
    let file_true = prefix.join("conf/mut_file_true.rss");
    write_script(
        &file_true,
        &format!(
            r#"
use ngx;
ngx::req::read_body();
ngx::req::set_body_file("{path}", true);
ngx::log(ngx::log_level::notice(), ngx::encode_args({{ file_true_cl: ngx::var::get("content_length") }}));
ngx::say(ngx::req::get_body_file());
"#,
            path = source_true_path.replace('\\', "\\\\")
        ),
    );
    let file_false = prefix.join("conf/mut_file_false.rss");
    write_script(
        &file_false,
        &format!(
            r#"
use ngx;
ngx::req::read_body();
ngx::req::set_body_file("{path}", false);
ngx::log(ngx::log_level::notice(), ngx::encode_args({{ file_false_cl: ngx::var::get("content_length") }}));
ngx::say(ngx::req::get_body_file());
"#,
            path = source_false_path.replace('\\', "\\\\")
        ),
    );
    let file_proxy = prefix.join("conf/mut_file_proxy.rss");
    write_script(
        &file_proxy,
        &format!(
            r#"
use ngx;
ngx::req::read_body();
ngx::req::set_body_file("{path}", false);
ngx::log(ngx::log_level::notice(), ngx::encode_args({{ file_proxy_cl: ngx::var::get("content_length") }}));
"#,
            path = source_false_path.replace('\\', "\\\\")
        ),
    );
    let builder = prefix.join("conf/mut_builder.rss");
    write_script(
        &builder,
        r#"
use ngx;
ngx::req::read_body();
ngx::req::init_body(null);
ngx::req::append_body("built-");
ngx::req::append_body("body");
ngx::req::finish_body();
ngx::log(ngx::log_level::notice(), ngx::encode_args({ builder_cl: ngx::var::get("content_length") }));
ngx::say(ngx::req::get_body_data());
"#,
    );
    let builder_proxy = prefix.join("conf/mut_builder_proxy.rss");
    write_script(
        &builder_proxy,
        r#"
use ngx;
ngx::req::read_body();
ngx::req::init_body(null);
ngx::req::append_body("built-body");
ngx::req::finish_body();
ngx::log(ngx::log_level::notice(), ngx::encode_args({ builder_proxy_cl: ngx::var::get("content_length") }));
"#,
    );
    let chunked_proxy = prefix.join("conf/mut_chunked_proxy.rss");
    write_script(
        &chunked_proxy,
        r#"
use ngx;
ngx::req::read_body();
ngx::req::set_body_data("from-chunked");
"#,
    );
    let ssi_access = prefix.join("conf/mut_ssi_access.rss");
    write_script(
        &ssi_access,
        r#"
use ngx;
ngx::req::read_body();
ngx::req::set_body_data("ssi-inherited");
"#,
    );
    let ssi_child = prefix.join("conf/mut_ssi_child.rss");
    write_script(
        &ssi_child,
        r#"
use ngx;
ngx::log(ngx::log_level::notice(), ngx::encode_args({ body: ngx::req::get_body_data() }));
ngx::say(ngx::req::get_body_data());
"#,
    );
    let html_dir = prefix.join("html");
    fs::create_dir_all(&html_dir).unwrap();
    fs::write(
        html_dir.join("ssi.html"),
        "SSI_CHILD=<!--# include virtual=\"/ssi-child\" -->\n",
    )
    .unwrap();
    let redir = prefix.join("conf/mut_redir.rss");
    write_script(
        &redir,
        r#"
use ngx;
ngx::req::read_body();
ngx::req::set_body_data("after-redirect");
ngx::exit(418);
"#,
    );
    let redir_inner = prefix.join("conf/mut_redir_inner.rss");
    write_script(
        &redir_inner,
        r#"
use ngx;
ngx::say(ngx::req::get_body_data());
"#,
    );
    let iso_replacements = [
        "iso0-unique-A",
        "iso1-unique-BB",
        "iso2-unique-CCC",
        "iso3-unique-DDDD",
    ];
    let iso_originals = [
        "orig0-stale",
        "orig1-staleX",
        "orig2-staleYY",
        "orig3-staleZZZ",
    ];
    let mut iso_locations = String::new();
    for (i, replacement) in iso_replacements.iter().enumerate() {
        let script = prefix.join(format!("conf/mut_iso_{i}.rss"));
        write_script(
            &script,
            &format!(
                r#"
use ngx;
ngx::req::read_body();
ngx::req::set_body_data("{replacement}");
"#
            ),
        );
        iso_locations.push_str(&format!(
            r#"
        location /iso-{i}/ {{
            access_by_rss_file {script};
            proxy_pass http://127.0.0.1:{echo_port};
            proxy_http_version 1.1;
            proxy_set_header Connection close;
        }}
"#,
            script = nginx_quote_path(&script).unwrap(),
            echo_port = echo_port,
        ));
    }

    let mut clean_sources = Vec::new();
    let mut clean_locations = String::new();
    for i in 0..4 {
        let source = prefix.join(format!("html/source-clean-{i}.txt"));
        fs::write(&source, format!("clean-payload-{i}")).unwrap();
        let source_path = source.display().to_string().replace('\\', "\\\\");
        let script = prefix.join(format!("conf/mut_file_clean_{i}.rss"));
        write_script(
            &script,
            &format!(
                r#"
use ngx;
ngx::req::read_body();
ngx::req::set_body_file("{source_path}", true);
ngx::say(ngx::req::get_body_file());
"#
            ),
        );
        clean_locations.push_str(&format!(
            r#"
        location /file-clean-{i}/ {{
            content_by_rss_file {script};
        }}
"#,
            script = nginx_quote_path(&script).unwrap(),
        ));
        clean_sources.push(source);
    }

    let abort = prefix.join("conf/mut_abort.rss");
    let abort_chunk = "X".repeat(32 * 1024);
    let mut abort_src = format!(
        r#"
use ngx;
ngx::req::read_body();
ngx::req::set_body_file("{path}", true);
ngx::say(ngx::req::get_body_file());
"#,
        path = source_true_path.replace('\\', "\\\\")
    );
    for _ in 0..8 {
        abort_src.push_str("ngx::print(\"");
        abort_src.push_str(&abort_chunk);
        abort_src.push_str("\");\n");
    }
    abort_src.push_str("ngx::flush(true);\nngx::say(\"late\");\n");
    write_script(&abort, &abort_src);

    let mut locations = format!(
        r#"
        location /mem/ {{
            content_by_rss_file {mem};
        }}
        location /mem-proxy/ {{
            access_by_rss_file {mem_proxy};
            proxy_pass http://127.0.0.1:{echo_port};
            proxy_http_version 1.1;
            proxy_set_header Connection close;
        }}
        location /file-true/ {{
            content_by_rss_file {file_true};
        }}
        location /file-false/ {{
            content_by_rss_file {file_false};
        }}
        location /file-proxy/ {{
            access_by_rss_file {file_proxy};
            proxy_pass http://127.0.0.1:{echo_port};
            proxy_http_version 1.1;
            proxy_set_header Connection close;
        }}
        location /builder/ {{
            content_by_rss_file {builder};
        }}
        location /builder-proxy/ {{
            access_by_rss_file {builder_proxy};
            proxy_pass http://127.0.0.1:{echo_port};
            proxy_http_version 1.1;
            proxy_set_header Connection close;
        }}
        location /chunked-proxy/ {{
            access_by_rss_file {chunked_proxy};
            proxy_pass http://127.0.0.1:{echo_port};
            proxy_http_version 1.1;
            proxy_set_header Connection close;
        }}
        location /ssi-parent/ {{
            access_by_rss_file {ssi_access};
            ssi on;
            ssi_types *;
            default_type text/html;
            try_files /ssi.html =404;
        }}
        location = /ssi-child {{
            content_by_rss_file {ssi_child};
        }}
        location /redir/ {{
            access_by_rss_file {redir};
            error_page 418 =200 /redir-inner;
        }}
        location /redir-inner {{
            internal;
            content_by_rss_file {redir_inner};
        }}
        location /abort/ {{
            postpone_output 0;
            content_by_rss_file {abort};
        }}
"#,
        mem = nginx_quote_path(&mem).unwrap(),
        mem_proxy = nginx_quote_path(&mem_proxy).unwrap(),
        file_true = nginx_quote_path(&file_true).unwrap(),
        file_false = nginx_quote_path(&file_false).unwrap(),
        file_proxy = nginx_quote_path(&file_proxy).unwrap(),
        builder = nginx_quote_path(&builder).unwrap(),
        builder_proxy = nginx_quote_path(&builder_proxy).unwrap(),
        chunked_proxy = nginx_quote_path(&chunked_proxy).unwrap(),
        ssi_access = nginx_quote_path(&ssi_access).unwrap(),
        ssi_child = nginx_quote_path(&ssi_child).unwrap(),
        redir = nginx_quote_path(&redir).unwrap(),
        redir_inner = nginx_quote_path(&redir_inner).unwrap(),
        abort = nginx_quote_path(&abort).unwrap(),
        echo_port = echo_port,
    );
    locations.push_str(&iso_locations);
    locations.push_str(&clean_locations);
    let conf = body_conf(&module, &prefix, port, &locations);
    fs::write(prefix.join("conf/nginx.conf"), &conf).unwrap();
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "body mutation scripts must pass nginx -t:\n{text}"
    );

    let mut running = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(15), prefix.path());

    let (status, response) = http_exchange(
        port,
        "POST /mem/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 7\r\nConnection: close\r\n\r\nstale-x",
    );
    assert_eq!(status, 200, "{response}");
    let (_, payload) = parse_http_response(&response);
    assert_eq!(payload, "replaced-memory\n");

    let (status, response) = http_exchange(
        port,
        "POST /mem-proxy/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 7\r\nConnection: close\r\n\r\nstale-x",
    );
    assert_eq!(status, 200, "{response}");
    let echo = echo_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("echo mem");
    assert_eq!(echo.method, "POST");
    assert_eq!(echo.content_length.as_deref(), Some("15"));
    assert_eq!(echo.content_length_count, 1);
    assert!(echo.transfer_encoding.is_none());
    assert_eq!(echo.body, b"replaced-memory");
    assert!(!echo.body.windows(7).any(|w| w == b"stale-x"));

    let (status, response) = http_exchange(
        port,
        "POST /builder/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\nold",
    );
    assert_eq!(status, 200, "{response}");
    let (_, payload) = parse_http_response(&response);
    assert_eq!(payload, "built-body\n");

    let (status, _response) = http_exchange(
        port,
        "POST /builder-proxy/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\nold",
    );
    assert_eq!(status, 200);
    let echo = echo_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("echo builder");
    assert_eq!(echo.body, b"built-body");
    assert_eq!(echo.content_length.as_deref(), Some("10"));
    assert_eq!(echo.content_length_count, 1);
    assert!(echo.transfer_encoding.is_none());

    let (status, _response) = http_exchange(
        port,
        "POST /chunked-proxy/ HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n7\r\nstale-x\r\n0\r\n\r\n",
    );
    assert_eq!(status, 200);
    let echo = echo_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("echo chunked");
    assert_eq!(echo.body, b"from-chunked");
    assert_eq!(echo.content_length.as_deref(), Some("12"));
    assert_eq!(echo.content_length_count, 1);
    assert!(
        echo.transfer_encoding.is_none(),
        "chunked TE must not be forwarded, got {:?}",
        echo.transfer_encoding
    );
    assert!(!echo.body.windows(7).any(|w| w == b"stale-x"));

    let (status, response) = http_exchange(
        port,
        "POST /file-proxy/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\nold",
    );
    assert_eq!(
        status,
        200,
        "file-proxy {response} err={}",
        fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default()
    );
    let echo = echo_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("echo file");
    assert_eq!(echo.body, b"file-false-payload");
    assert_eq!(echo.content_length.as_deref(), Some("18"));
    assert_eq!(echo.content_length_count, 1);
    assert!(echo.transfer_encoding.is_none());

    let (status, response) = http_exchange(
        port,
        "POST /file-true/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\nold",
    );
    assert_eq!(status, 200, "{response}");
    let (_, copy_path) = parse_http_response(&response);
    let copy_path = copy_path.trim().to_string();
    assert!(
        Path::new(&copy_path) != source_true.as_path(),
        "auto_clean must not expose the caller source path: {copy_path}"
    );
    assert!(
        source_true.exists(),
        "auto_clean must leave the caller source untouched"
    );
    assert!(
        wait_path_absent(Path::new(&copy_path), Duration::from_secs(3)),
        "file-true auto_clean copy must disappear after request pool cleanup: {copy_path}"
    );

    let (status, response) = http_exchange(
        port,
        "POST /file-false/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\nold",
    );
    assert_eq!(status, 200, "{response}");
    let (_, false_path) = parse_http_response(&response);
    assert!(
        false_path.contains("source-false.txt"),
        "auto_clean=false should keep source path, got {false_path}"
    );
    assert!(source_false.exists());

    let cl_log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    for needle in [
        "mem_cl=15",
        "mem_proxy_cl=15",
        "builder_cl=10",
        "builder_proxy_cl=10",
        "file_true_cl=17",
        "file_false_cl=18",
        "file_proxy_cl=18",
    ] {
        assert!(
            cl_log.contains(needle),
            "live $content_length via ngx::var::get missing {needle}: {cl_log}"
        );
    }

    let (status, response) = http_exchange(
        port,
        "GET /ssi-parent/ HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(
        status,
        200,
        "ssi-parent {response} err={}",
        fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default()
    );
    assert!(
        !response.contains("<!--#"),
        "SSI include must expand, got {response}"
    );
    let (_, payload) = parse_http_response(&response);
    assert!(
        payload.contains("ssi-inherited"),
        "SSI child getter must return inherited mutated body: {payload:?}"
    );
    let error_log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert!(
        error_log.contains("body=ssi-inherited") || error_log.contains("ssi-inherited"),
        "SSI child must log getter value, err={error_log}"
    );

    let (status, response) = http_exchange(
        port,
        "POST /redir/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\nold",
    );
    assert_eq!(status, 200, "{response}");
    let (_, payload) = parse_http_response(&response);
    assert_eq!(payload, "after-redirect\n");

    let mut handles = Vec::new();
    for (i, original) in iso_originals.iter().copied().enumerate() {
        let handle = std::thread::spawn(move || {
            let (status, response) = http_exchange(
                port,
                &format!(
                    "POST /iso-{i}/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{original}",
                    original.len()
                ),
            );
            (i, status, response)
        });
        handles.push(handle);
    }
    for handle in handles {
        let (i, status, response) = handle.join().unwrap();
        assert_eq!(status, 200, "iso-{i} {response}");
    }
    let mut seen_iso = HashSet::new();
    for _ in 0..4 {
        let echo = echo_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("echo iso");
        let Some((i, replacement)) = iso_replacements
            .iter()
            .enumerate()
            .find(|(i, _)| {
                echo.request_target == format!("/iso-{i}/")
                    || echo.request_target.starts_with(&format!("/iso-{i}/"))
            })
            .map(|(i, body)| (i, *body))
        else {
            panic!(
                "unexpected iso upstream target {} body {:?}",
                echo.request_target,
                String::from_utf8_lossy(&echo.body)
            );
        };
        assert!(
            seen_iso.insert(i),
            "duplicate upstream record for /iso-{i}/"
        );
        assert_eq!(echo.method, "POST");
        assert_eq!(echo.body, replacement.as_bytes(), "iso-{i} body");
        let expect_cl = replacement.len().to_string();
        assert_eq!(
            echo.content_length.as_deref(),
            Some(expect_cl.as_str()),
            "iso-{i} content-length"
        );
        assert_eq!(
            echo.content_length_count, 1,
            "iso-{i} must have a single CL"
        );
        assert!(
            echo.transfer_encoding.is_none(),
            "iso-{i} must not forward TE, got {:?}",
            echo.transfer_encoding
        );
        assert!(
            !echo
                .body
                .windows(iso_originals[i].len())
                .any(|w| w == iso_originals[i].as_bytes()),
            "iso-{i} must not forward original body"
        );
    }
    assert_eq!(seen_iso.len(), 4, "each iso path must appear once");

    let mut clean_handles = Vec::new();
    for i in 0..4 {
        let handle = std::thread::spawn(move || {
            let (status, response) = http_exchange(
                port,
                &format!(
                    "POST /file-clean-{i}/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\nold"
                ),
            );
            (i, status, response)
        });
        clean_handles.push(handle);
    }
    let mut clean_copies = Vec::new();
    for handle in clean_handles {
        let (i, status, response) = handle.join().unwrap();
        assert_eq!(
            status,
            200,
            "file-clean-{i} {response} err={}",
            fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default()
        );
        let (_, payload) = parse_http_response(&response);
        let copy = payload.trim().to_string();
        assert!(
            copy.contains("ngx-rss-body-"),
            "file-clean-{i} must return private copy path, got {copy}"
        );
        assert!(
            Path::new(&copy) != clean_sources[i].as_path(),
            "file-clean-{i} must not expose source path"
        );
        clean_copies.push(copy);
    }
    let unique: HashSet<&str> = clean_copies.iter().map(String::as_str).collect();
    assert_eq!(
        unique.len(),
        clean_copies.len(),
        "concurrent auto_clean copies must be unique: {clean_copies:?}"
    );
    for source in &clean_sources {
        assert!(
            source.exists(),
            "auto_clean source must survive: {}",
            source.display()
        );
    }
    for copy in &clean_copies {
        assert!(
            wait_path_absent(Path::new(copy), Duration::from_secs(3)),
            "concurrent auto_clean copy must disappear after request pool cleanup: {copy}"
        );
    }

    let (abort_stream, abort_copy, abort_buf) =
        capture_abort_copy_path(port, Duration::from_secs(3));
    assert!(
        abort_copy.contains("ngx-rss-body-"),
        "abort must flush private copy path, got {abort_copy} from {}",
        String::from_utf8_lossy(&abort_buf)
    );
    assert!(
        Path::new(&abort_copy) != source_true.as_path(),
        "abort copy must not be the caller source"
    );
    assert!(
        PathBuf::from(&abort_copy).exists(),
        "abort copy must exist while the request is still active: {abort_copy}"
    );
    assert!(source_true.exists(), "abort must not unlink caller source");
    assert!(
        wait_log_contains(
            &prefix.join("logs/error.log"),
            "FLUSH_WAIT_ARMING",
            Duration::from_secs(3)
        ),
        "abort flush wait must arm before disconnect; error:\n{}",
        fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default()
    );
    assert!(
        PathBuf::from(&abort_copy).exists(),
        "abort copy must remain while flush wait is armed: {abort_copy}"
    );
    drop(abort_stream);
    assert!(
        wait_access_status(
            &prefix.join("logs/access.log"),
            "POST",
            "/abort/",
            499,
            Duration::from_secs(5)
        ),
        "abort disconnect must access-log 499 before SIGQUIT; access:\n{}\nerror:\n{}",
        fs::read_to_string(prefix.join("logs/access.log")).unwrap_or_default(),
        fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default()
    );
    assert!(
        wait_path_absent(Path::new(&abort_copy), Duration::from_secs(3)),
        "abort auto_clean copy must disappear after request finalize, before SIGQUIT: {abort_copy}"
    );
    assert!(
        source_true.exists(),
        "abort auto_clean must leave caller source after copy unlink"
    );

    let reload = Command::new(&nginx)
        .arg("-p")
        .arg(prefix.path())
        .arg("-c")
        .arg("conf/nginx.conf")
        .arg("-s")
        .arg("reload")
        .output()
        .expect("reload");
    assert!(
        reload.status.success(),
        "reload failed: {}",
        String::from_utf8_lossy(&reload.stderr)
    );
    std::thread::sleep(Duration::from_millis(400));
    let (status, response) = http_exchange(
        port,
        "POST /mem/ HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\nrel",
    );
    assert_eq!(status, 200, "healthy after reload: {response}");

    quit_and_wait(&mut running.child, Duration::from_secs(8));
    drop(echo_thread);

    assert!(
        source_true.exists(),
        "auto_clean source must survive request cleanup"
    );
    assert!(
        source_false.exists(),
        "auto_clean=false source must survive"
    );
    if !copy_path.is_empty() {
        assert!(
            !PathBuf::from(&copy_path).exists(),
            "module-created auto_clean copy must disappear after request: {copy_path}"
        );
    }
}
