//! Config-time `log_by_rss_file` slots, inheritance, and Log allowlist.

use ngx_rustscript_nginx_test_stubs as _;

#[path = "support/temp.rs"]
mod temp;

use std::path::Path;
use std::sync::Arc;

use ngx::http::Merge;
use vm::Program;

use ngx_rustscript::config::{
    ConfigError, LocationConf, compile_rss_source_for_phase, load_and_compile_rss_file,
    load_and_compile_rss_file_for_phase,
};
use ngx_rustscript::host::{
    HostCapability, HostFamily, HostManifestEntry, HostSyncKind, LOG_ALLOWED_NAMES, NgxPhase,
    host_validates_for_log, log_allowlist_accepts, ngx_host_manifest,
};
use ngx_rustscript::phase::ScriptLocationConf;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn program_ptr(program: &Arc<Program>) -> *const Program {
    Arc::as_ptr(program)
}

fn compile_log(source: &str) -> Result<Arc<Program>, ConfigError> {
    compile_rss_source_for_phase(source, NgxPhase::Log)
}

fn compile_log_err(source: &str) -> ConfigError {
    compile_log(source).expect_err("log compile should fail")
}

#[test]
fn production_host_count_stays_199() {
    assert_eq!(ngx_host_manifest().len(), 199);
}

#[test]
fn location_can_hold_access_content_and_log_independently() {
    let access = load_and_compile_rss_file(&fixture("ok.rss")).expect("access");
    let content =
        load_and_compile_rss_file_for_phase(&fixture("runtime_error.rss"), NgxPhase::Content)
            .expect("content");
    let log = load_and_compile_rss_file_for_phase(&fixture("ok.rss"), NgxPhase::Log).expect("log");
    let mut conf = LocationConf::default();
    conf.set_access_program(access.clone()).unwrap();
    conf.set_content_program(content.clone()).unwrap();
    conf.set_log_program(log.clone()).unwrap();
    assert_eq!(
        program_ptr(conf.access_program().expect("access set")),
        program_ptr(&access)
    );
    assert_eq!(
        program_ptr(conf.content_program().expect("content set")),
        program_ptr(&content)
    );
    assert_eq!(
        program_ptr(conf.log_program().expect("log set")),
        program_ptr(&log)
    );
}

#[test]
fn log_duplicate_is_independent_of_access_and_content() {
    let access = load_and_compile_rss_file(&fixture("ok.rss")).unwrap();
    let content =
        load_and_compile_rss_file_for_phase(&fixture("ok.rss"), NgxPhase::Content).unwrap();
    let log = load_and_compile_rss_file_for_phase(&fixture("ok.rss"), NgxPhase::Log).unwrap();
    let mut conf = LocationConf::default();
    conf.set_access_program(access).unwrap();
    conf.set_content_program(content).unwrap();
    conf.set_log_program(log.clone()).unwrap();
    let dup = conf.set_log_program(log).expect_err("log dup");
    assert!(matches!(dup, ConfigError::DuplicateLog), "got {dup:?}");
    assert!(conf.access_program().is_some());
    assert!(conf.content_program().is_some());
    assert!(conf.log_program().is_some());
}

#[test]
fn unset_child_inherits_log_independently() {
    let access = load_and_compile_rss_file(&fixture("ok.rss")).unwrap();
    let content =
        load_and_compile_rss_file_for_phase(&fixture("runtime_error.rss"), NgxPhase::Content)
            .unwrap();
    let log = load_and_compile_rss_file_for_phase(&fixture("ok.rss"), NgxPhase::Log).unwrap();
    let mut parent = LocationConf::default();
    parent.set_access_program(access.clone()).unwrap();
    parent.set_content_program(content.clone()).unwrap();
    parent.set_log_program(log.clone()).unwrap();
    let mut child = LocationConf::default();
    child.merge(&parent).unwrap();
    assert_eq!(
        program_ptr(child.access_program().expect("inherited access")),
        program_ptr(&access)
    );
    assert_eq!(
        program_ptr(child.content_program().expect("inherited content")),
        program_ptr(&content)
    );
    assert_eq!(
        program_ptr(child.log_program().expect("inherited log")),
        program_ptr(&log)
    );
}

#[test]
fn explicit_child_log_overrides_parent_without_clobbering_other_slots() {
    let parent_access = load_and_compile_rss_file(&fixture("ok.rss")).unwrap();
    let parent_content =
        load_and_compile_rss_file_for_phase(&fixture("runtime_error.rss"), NgxPhase::Content)
            .unwrap();
    let parent_log =
        load_and_compile_rss_file_for_phase(&fixture("ok.rss"), NgxPhase::Log).unwrap();
    let child_log =
        load_and_compile_rss_file_for_phase(&fixture("runtime_error.rss"), NgxPhase::Log).unwrap();
    let mut parent = LocationConf::default();
    parent.set_access_program(parent_access.clone()).unwrap();
    parent.set_content_program(parent_content.clone()).unwrap();
    parent.set_log_program(parent_log).unwrap();
    let mut child = LocationConf::default();
    child.set_log_program(child_log.clone()).unwrap();
    child.merge(&parent).unwrap();
    assert_eq!(
        program_ptr(child.access_program().expect("inherited access")),
        program_ptr(&parent_access)
    );
    assert_eq!(
        program_ptr(child.content_program().expect("inherited content")),
        program_ptr(&parent_content)
    );
    assert_eq!(
        program_ptr(child.log_program().expect("override log")),
        program_ptr(&child_log)
    );
}

#[test]
fn script_location_conf_log_slot_merges_independently() {
    let parent_log =
        load_and_compile_rss_file_for_phase(&fixture("ok.rss"), NgxPhase::Log).expect("parent log");
    let child_log =
        load_and_compile_rss_file_for_phase(&fixture("runtime_error.rss"), NgxPhase::Log)
            .expect("child log");
    let mut parent = ScriptLocationConf::default();
    parent.set_program(parent_log.clone()).unwrap();
    let mut inherited = ScriptLocationConf::default();
    inherited.merge_from(&parent);
    assert_eq!(
        program_ptr(inherited.program().expect("inherited")),
        program_ptr(&parent_log)
    );
    let mut overridden = ScriptLocationConf::default();
    overridden.set_program(child_log.clone()).unwrap();
    overridden.merge_from(&parent);
    assert_eq!(
        program_ptr(overridden.program().expect("kept")),
        program_ptr(&child_log)
    );
}

#[test]
fn log_phase_accepts_sync_read_log_time_meta_ctx_and_shared_dict() {
    compile_log("use ngx;\nngx::log(ngx::log_level::notice(), \"ok\");\n").expect("ngx::log");
    compile_log("use ngx;\nngx::get_phase();\n").expect("get_phase");
    compile_log("use ngx;\nngx::var::get(\"uri\");\n").expect("var get");
    compile_log("use ngx;\nngx::now();\nngx::time();\n").expect("time");
    compile_log("use ngx;\nngx::escape_uri(\"a\", null);\n").expect("codec");
    compile_log("use ngx;\nngx::status::get();\n").expect("status get");
    compile_log("use ngx;\nngx::header::get(\"Content-Type\");\n").expect("header get");
    compile_log("use ngx;\nngx::ctx::set(\"k\", \"v\");\nngx::ctx::get(\"k\");\n").expect("ctx");
    compile_log("use ngx;\nngx::shared::dict(\"dogs\");\n").expect("shared dict lookup");
    let ctx = compile_log("use ngx;\nngx::ctx::set(\"k\", 1);\n").expect("ctx-only");
    assert!(ngx_rustscript::config::program_requires_request_state(&ctx));
}

#[test]
fn log_phase_rejects_print_and_runtime_sleep() {
    let err = compile_log(include_str!("fixtures/print.rss")).expect_err("print");
    let text = err.to_string().to_ascii_lowercase();
    assert!(text.contains("print"), "got {err}");
    let err = compile_log(include_str!("fixtures/runtime_sleep.rss")).expect_err("runtime::sleep");
    let text = err.to_string().to_ascii_lowercase();
    assert!(text.contains("runtime::sleep"), "got {err}");
}

#[test]
fn log_phase_rejects_sleep_udp_tcp_output_control_and_mutation() {
    let cases: &[(&str, &str)] = &[
        ("use ngx;\nngx::sleep(0.01);\n", "ngx::sleep"),
        ("use ngx;\nngx::sleep_ms(1);\n", "ngx::sleep_ms"),
        ("use ngx;\nngx::socket::udp();\n", "ngx::socket::udp"),
        ("use ngx;\nngx::socket::tcp();\n", "ngx::socket::tcp"),
        (
            "use ngx;\nngx::socket::connect(\"127.0.0.1\", 80);\n",
            "ngx::socket::connect",
        ),
        ("use ngx;\nngx::say(\"x\");\n", "ngx::say"),
        ("use ngx;\nngx::print(\"x\");\n", "ngx::print"),
        ("use ngx;\nngx::exit(200);\n", "ngx::exit"),
        ("use ngx;\nngx::redirect(\"/\", 302);\n", "ngx::redirect"),
        (
            "use ngx;\nngx::req::set_header(\"X-A\", \"b\");\n",
            "ngx::req::set_header",
        ),
        (
            "use ngx;\nngx::req::clear_header(\"X-A\");\n",
            "ngx::req::clear_header",
        ),
        (
            "use ngx;\nngx::req::set_method(1);\n",
            "ngx::req::set_method",
        ),
        (
            "use ngx;\nngx::req::set_uri(\"/x\", false);\n",
            "ngx::req::set_uri",
        ),
        ("use ngx;\nngx::status::set(201);\n", "ngx::status::set"),
        (
            "use ngx;\nngx::header::set(\"X-A\", \"b\");\n",
            "ngx::header::set",
        ),
        (
            "use ngx;\nngx::header::set_all(\"X-A\", [\"b\"]);\n",
            "ngx::header::set_all",
        ),
        (
            "use ngx;\nngx::header::clear(\"X-A\");\n",
            "ngx::header::clear",
        ),
        (
            "use ngx;\nngx::var::set(\"writable\", \"1\");\n",
            "ngx::var::set",
        ),
    ];
    for (source, needle) in cases {
        let err = compile_log_err(source);
        let text = err.to_string();
        assert!(
            text.contains(needle) || text.to_ascii_lowercase().contains("not allowed"),
            "{needle} must fail Log compile, got {err}"
        );
        match err {
            ConfigError::PhaseForbidden { name, phase } => {
                assert_eq!(phase, NgxPhase::Log, "{needle}");
                assert!(
                    name.contains(
                        needle
                            .trim_start_matches("ngx::")
                            .split("::")
                            .next()
                            .unwrap_or(needle)
                    ) || name == *needle
                        || text.contains(needle),
                    "{needle} PhaseForbidden name={name}"
                );
            }
            ConfigError::UnsupportedImport(names) => {
                assert!(
                    names.contains(needle) || names.contains("say") || names.contains("print"),
                    "{needle} UnsupportedImport={names}"
                );
            }
            ConfigError::Compile(msg) => {
                assert!(
                    msg.contains(needle)
                        || needle.contains("say")
                        || needle.contains("print")
                        || msg.contains("not allowed")
                        || msg.contains("no host function")
                        || msg.contains("no overload"),
                    "{needle} compile={msg}"
                );
            }
            other => {
                panic!("{needle}: expected PhaseForbidden/UnsupportedImport/Compile, got {other:?}")
            }
        }
    }
}

#[test]
fn closed_log_allowlist_covers_all_199_hosts_and_rejects_future_sync_log() {
    assert_eq!(ngx_host_manifest().len(), 199);
    let mut allowed = 0usize;
    let mut rejected = 0usize;
    for entry in ngx_host_manifest() {
        let listed = LOG_ALLOWED_NAMES.contains(&entry.name);
        let validates = host_validates_for_log(entry);
        assert_eq!(
            validates, listed,
            "{} validates={validates} listed={listed} family={:?} caps={:?}",
            entry.name, entry.family, entry.capabilities
        );
        if validates {
            allowed += 1;
            assert_eq!(entry.sync, HostSyncKind::Sync, "{}", entry.name);
            assert!(
                entry.phases.contains(&NgxPhase::Log),
                "{} listed for Log must include Log in catalog phases",
                entry.name
            );
        } else {
            rejected += 1;
        }
    }
    for name in LOG_ALLOWED_NAMES {
        assert!(
            ngx_rustscript::host::manifest_entry(name).is_some(),
            "{name} must exist in the 199-host catalog"
        );
    }
    assert_eq!(allowed + rejected, 199);
    assert_eq!(allowed, LOG_ALLOWED_NAMES.len());
    assert!(allowed > 0 && rejected > 0);

    static FUTURE_PHASES: &[NgxPhase] = &[NgxPhase::Log, NgxPhase::Access];
    static TIME_CAPS: &[HostCapability] = &[HostCapability::Time];
    static WRITE_CAPS: &[HostCapability] = &[HostCapability::RequestWrite];
    static CONTROL_CAPS: &[HostCapability] = &[HostCapability::Control];
    static EMPTY_CAPS: &[HostCapability] = &[];

    let future_time = HostManifestEntry {
        name: "ngx::future::sync_log",
        family: HostFamily::Time,
        sync: HostSyncKind::Sync,
        phases: FUTURE_PHASES,
        capabilities: TIME_CAPS,
        openresty_name: "ngx.future",
    };
    assert!(
        !host_validates_for_log(&future_time),
        "unknown name in an allowed family must be rejected"
    );
    assert!(!log_allowlist_accepts(
        "ngx::future::sync_log",
        HostFamily::Time,
        HostSyncKind::Sync,
        FUTURE_PHASES,
        TIME_CAPS,
    ));

    assert!(!log_allowlist_accepts(
        "ngx::future::body",
        HostFamily::Control,
        HostSyncKind::Sync,
        FUTURE_PHASES,
        EMPTY_CAPS,
    ));
    assert!(!log_allowlist_accepts(
        "ngx::req::get_method",
        HostFamily::Request,
        HostSyncKind::Sync,
        FUTURE_PHASES,
        WRITE_CAPS,
    ));
    assert!(!log_allowlist_accepts(
        "ngx::future::scheduler",
        HostFamily::Timer,
        HostSyncKind::Sync,
        FUTURE_PHASES,
        EMPTY_CAPS,
    ));
    assert!(!log_allowlist_accepts(
        "ngx::future::control",
        HostFamily::Control,
        HostSyncKind::Sync,
        FUTURE_PHASES,
        CONTROL_CAPS,
    ));
}

#[test]
fn no_async_host_validates_for_log_across_all_hosts() {
    let mut seen_async = 0usize;
    for entry in ngx_host_manifest() {
        let async_like = entry.sync == HostSyncKind::Async
            || entry.capabilities.contains(&HostCapability::Timer)
            || entry.capabilities.contains(&HostCapability::Cosocket);
        if async_like {
            seen_async += 1;
            assert!(
                !host_validates_for_log(entry),
                "{} must not validate for log_by_rss_file",
                entry.name
            );
        }
        if entry.sync == HostSyncKind::Async {
            assert!(
                !host_validates_for_log(entry),
                "async {} must not validate for Log",
                entry.name
            );
        }
    }
    assert!(
        seen_async > 0,
        "catalog must include async hosts to lock the Log invariant"
    );
    assert_eq!(ngx_host_manifest().len(), 199);
}

#[test]
fn mutation_and_control_hosts_do_not_validate_for_log_even_if_openresty_lists_log() {
    for name in [
        "ngx::req::set_header",
        "ngx::req::clear_header",
        "ngx::req::set_body_data",
        "ngx::req::set_body_file",
        "ngx::req::init_body",
        "ngx::req::append_body",
        "ngx::req::finish_body",
        "ngx::status::set",
        "ngx::header::set",
        "ngx::header::set_all",
        "ngx::header::clear",
        "ngx::var::set",
        "ngx::exit",
        "ngx::redirect",
        "ngx::sleep",
        "ngx::sleep_ms",
        "ngx::socket::udp",
        "ngx::socket::tcp",
    ] {
        let entry = ngx_rustscript::host::manifest_entry(name).expect(name);
        assert!(
            !host_validates_for_log(entry),
            "{name} must not validate for executable Log"
        );
    }
    for (source, name) in [
        (
            "use ngx;\nngx::req::set_header(\"X\", \"y\");\n",
            "ngx::req::set_header",
        ),
        ("use ngx;\nngx::status::set(201);\n", "ngx::status::set"),
        ("use ngx;\nngx::exit(200);\n", "ngx::exit"),
        ("use ngx;\nngx::sleep_ms(1);\n", "ngx::sleep_ms"),
    ] {
        match compile_log(source) {
            Err(ConfigError::PhaseForbidden { name: got, phase }) => {
                assert_eq!(got, name);
                assert_eq!(phase, NgxPhase::Log);
            }
            Err(ConfigError::UnsupportedImport(msg)) => {
                assert!(msg.contains(name), "{name} got {msg}");
            }
            other => panic!("{name} must be rejected at Log config time, got {other:?}"),
        }
    }
}

#[test]
fn default_compile_rss_source_stays_access_phase() {
    ngx_rustscript::config::compile_rss_source("use ngx;\nngx::sleep_ms(1);\n")
        .expect("access still accepts sleep");
}

#[test]
fn log_runtime_ops_block_var_status_header_mutation_when_config_is_bypassed() {
    use std::mem::MaybeUninit;

    use nginx_sys::ngx_http_request_t;
    use ngx_rustscript::host::{AllowedOps, bind_registry_for_test, with_request_scope};
    use ngx_rustscript::notify::RequestIdentity;
    use vm::{Vm, VmStatus};

    fn dummy_request() -> Box<ngx_http_request_t> {
        let mut req =
            Box::new(unsafe { MaybeUninit::<ngx_http_request_t>::zeroed().assume_init() });
        let ptr = (&mut *req) as *mut ngx_http_request_t;
        req.main = ptr;
        req
    }

    fn run_under_log(source: &str) -> Result<VmStatus, String> {
        let program = ngx_rustscript::config::compile_rss_source(source).unwrap_or_else(|err| {
            panic!("access compile must succeed to bypass log config: {err}")
        });
        let mut vm = Vm::try_new((*program).clone()).expect("vm");
        bind_registry_for_test(&mut vm).expect("bind test registry");
        let mut request = dummy_request();
        with_request_scope(
            (&mut *request) as *mut ngx_http_request_t,
            RequestIdentity::new(1, 1),
            NgxPhase::Log,
            AllowedOps::for_phase(NgxPhase::Log),
            || match vm.run() {
                Ok(status) => Ok(status),
                Err(err) => Err(err.to_string()),
            },
        )
    }

    for (source, needle) in [
        ("use ngx;\nngx::var::set(\"args\", \"x\");\n", "not allowed"),
        ("use ngx;\nngx::status::set(201);\n", "not allowed"),
        (
            "use ngx;\nngx::header::set(\"X-Log\", \"no\");\n",
            "not allowed",
        ),
    ] {
        let err = run_under_log(source).expect_err(source);
        let text = err.to_ascii_lowercase();
        assert!(
            text.contains(needle) || text.contains("opsnotallowed"),
            "{source} expected ops rejection, got {err}"
        );
    }
}
