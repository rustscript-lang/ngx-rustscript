//! NGINX ABI pin, config quoting, and `nginx -t` helpers.

#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use super::temp::{env_path, isolated_prefix};

#[path = "../../support/shell_words.rs"]
mod shell_words;
use shell_words::split_shell_words;

pub const PINNED_NGINX_VERSION_BANNER: &str = "openresty/1.31.4.1";
pub const REQUIRED_NGINX_CONFIGURE_ARGS: &[&str] = &["--with-http_ssl_module"];

pub fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}

pub fn nginx_process_group_command(nginx: &Path) -> Command {
    let mut command = Command::new(nginx);
    configure_process_group(&mut command);
    command
}

fn process_group_exists(pgid: i32) -> bool {
    let rc = unsafe { libc::kill(-pgid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn signal_process_group(pgid: i32, signal: i32) {
    let _ = unsafe { libc::kill(-pgid, signal) };
}

pub fn terminate_process_group(child: &mut Child, timeout: Duration) {
    let pgid = child.id() as i32;
    if process_group_exists(pgid) {
        signal_process_group(pgid, libc::SIGQUIT);
    }

    let deadline = Instant::now() + timeout;
    while process_group_exists(pgid) && Instant::now() < deadline {
        let _ = child.try_wait();
        std::thread::sleep(Duration::from_millis(10));
    }
    if process_group_exists(pgid) {
        signal_process_group(pgid, libc::SIGKILL);
    }

    let reap_deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) if Instant::now() < reap_deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

/// Wait until a worker answers HTTP.
///
/// Master listen sockets accept TCP before `init_process` finishes installing
/// the host registry. Latency and short-timeout HTTP helpers must not start
/// until a worker has entered its event loop.
pub fn wait_for_worker_http(port: u16, timeout: Duration) {
    wait_for_workers_ready(port, timeout, None);
}

/// Wait until every forked worker has logged `worker runtime ready`.
///
/// One worker can answer HTTP while siblings are still in `init_process`.
/// Multi-worker tests must not start until `error.log` shows a ready line for
/// each `start worker process`.
pub fn wait_for_workers_ready(port: u16, timeout: Duration, error_log: Option<&Path>) {
    let timeout = timeout.max(Duration::from_secs(15));
    let start = Instant::now();
    let mut last: Option<String> = None;
    let mut http_ok = false;
    loop {
        if !http_ok {
            match probe_worker_http(port) {
                Ok(()) => http_ok = true,
                Err(err) => last = Some(err),
            }
        }
        if http_ok {
            match error_log {
                None => return,
                Some(path) => match worker_ready_progress(path) {
                    Ok(()) => return,
                    Err(err) => last = Some(err),
                },
            }
        }
        if start.elapsed() >= timeout {
            panic!(
                "nginx worker did not become ready on 127.0.0.1:{port} within {timeout:?}: {}",
                last.as_deref().unwrap_or("port not yet listening")
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `Ok` when `error.log` shows every forked worker is ready.
///
/// `master_process off` never logs `start worker process`; HTTP readiness is
/// then enough.
pub fn worker_ready_progress(error_log: &Path) -> Result<(), String> {
    let text = fs::read_to_string(error_log).unwrap_or_default();
    let started = text.matches("start worker process ").count();
    let ready = text.matches("ngx-rustscript: worker runtime ready").count();
    if started == 0 || ready >= started {
        Ok(())
    } else {
        Err(format!(
            "workers started={started} ready={ready} log={}",
            error_log.display()
        ))
    }
}

fn probe_worker_http(port: u16) -> Result<(), String> {
    // Invalid Content-Length is rejected in header parsing (HTTP 400) before
    // rewrite/access/content, so readiness does not run RSS or consume
    // shared-dict / rate-limit quota.
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|err| format!("connect 127.0.0.1:{port}: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|err| format!("read timeout: {err}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .map_err(|err| format!("write timeout: {err}"))?;
    write!(
        stream,
        "GET / HTTP/1.0\r\nContent-Length: not-a-number\r\nConnection: close\r\n\r\n"
    )
    .map_err(|err| format!("write: {err}"))?;
    let _ = stream.flush();
    let mut body = Vec::new();
    stream
        .read_to_end(&mut body)
        .map_err(|err| format!("read: {err}"))?;
    if body.starts_with(b"HTTP/") {
        Ok(())
    } else {
        Err(format!("non-HTTP: {}", String::from_utf8_lossy(&body)))
    }
}

pub fn nginx_binary() -> PathBuf {
    let source_dir = env_path("NGINX_SOURCE_DIR").unwrap_or_else(|| {
        panic!(
            "NGINX_SOURCE_DIR must be set to the pinned configured OpenResty 1.31.4.1 source tree"
        )
    });
    if !source_dir.is_dir() {
        panic!(
            "NGINX_SOURCE_DIR is not a directory: {}",
            source_dir.display()
        );
    }
    let build_dir = env_path("NGINX_BUILD_DIR").unwrap_or_else(|| {
        panic!("NGINX_BUILD_DIR must be set to the pinned OpenResty 1.31.4.1 build directory")
    });
    let nginx = resolve_nginx_binary(&build_dir, env_path("NGX_RUSTSCRIPT_NGINX").as_deref())
        .unwrap_or_else(|err| panic!("{err}"));
    let output = Command::new(&nginx)
        .arg("-V")
        .output()
        .unwrap_or_else(|err| panic!("failed to spawn nginx -V: {err}"));
    let text = combined(&output);
    if !output.status.success() {
        panic!("nginx -V failed:\n{text}");
    }
    let report = parse_nginx_v_output(&text).unwrap_or_else(|err| panic!("{err}"));
    validate_nginx_version_report(&report).unwrap_or_else(|err| panic!("{err}"));
    nginx
}

pub fn resolve_nginx_binary(
    build_dir: &Path,
    override_nginx: Option<&Path>,
) -> Result<PathBuf, String> {
    if !build_dir.is_dir() {
        return Err(format!(
            "NGINX_BUILD_DIR is not a directory: {}",
            build_dir.display()
        ));
    }
    let selected = override_nginx
        .map(Path::to_path_buf)
        .unwrap_or_else(|| build_dir.join("nginx"));
    let canonical = fs::canonicalize(&selected).map_err(|err| {
        format!(
            "canonical selected nginx binary ({}): {err}",
            selected.display()
        )
    })?;
    let meta = fs::metadata(&canonical).map_err(|err| {
        format!(
            "stat selected nginx binary ({}): {err}",
            canonical.display()
        )
    })?;
    if !meta.is_file() {
        return Err(format!(
            "selected nginx binary is not a file: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NginxVersionReport {
    version: String,
    configure_args: Vec<String>,
}

pub fn parse_nginx_v_output(text: &str) -> Result<NginxVersionReport, String> {
    let mut version = None;
    let mut configure_args = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("nginx version:") {
            version = Some(rest.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("configure arguments:") {
            configure_args = Some(split_shell_words(rest)?);
        }
    }
    Ok(NginxVersionReport {
        version: version.ok_or_else(|| "nginx -V missing version line".to_string())?,
        configure_args: configure_args
            .ok_or_else(|| "nginx -V missing configure arguments".to_string())?,
    })
}

pub fn validate_nginx_version_report(report: &NginxVersionReport) -> Result<(), String> {
    if report.version != PINNED_NGINX_VERSION_BANNER {
        return Err(format!(
            "nginx version is {}, expected {PINNED_NGINX_VERSION_BANNER}",
            report.version
        ));
    }
    for required in REQUIRED_NGINX_CONFIGURE_ARGS {
        if !report.configure_args.iter().any(|arg| arg == required) {
            return Err(format!("nginx configure arguments missing {required}"));
        }
    }
    for arg in &report.configure_args {
        validate_configure_arg(arg)?;
    }
    Ok(())
}

const PATH_SHAPED_CONFIGURE_FLAGS: &[&str] = &[
    "--prefix=",
    "--sbin-path=",
    "--modules-path=",
    "--conf-path=",
    "--error-log-path=",
    "--pid-path=",
    "--lock-path=",
    "--http-log-path=",
    "--http-client-body-temp-path=",
    "--http-proxy-temp-path=",
    "--http-fastcgi-temp-path=",
    "--http-uwsgi-temp-path=",
    "--http-scgi-temp-path=",
    "--builddir=",
    "--add-module=",
    "--add-dynamic-module=",
];

fn validate_configure_arg(arg: &str) -> Result<(), String> {
    if arg.is_empty() || !arg.starts_with("--") {
        return Err(format!("unexpected nginx configure argument {arg}"));
    }
    for flag in PATH_SHAPED_CONFIGURE_FLAGS {
        if let Some(value) = arg.strip_prefix(flag) {
            validate_configure_path_value(flag, value)?;
            return Ok(());
        }
    }
    Ok(())
}

fn validate_configure_path_value(flag: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{flag} is missing a path"));
    }
    if value.bytes().any(|b| b == 0 || b < 0x20) {
        return Err(format!("{flag} path contains a control character"));
    }
    if value.starts_with('-') {
        return Err(format!("{flag} path must be path-shaped, not a flag"));
    }
    Ok(())
}

pub fn nginx_escape_quoted(value: &str) -> Result<String, String> {
    if value.bytes().any(|b| b == 0) {
        return Err("nginx config value contains NUL".to_string());
    }
    let mut out = String::new();
    for c in value.chars() {
        match c {
            '\n' | '\r' => return Err("nginx config value contains a newline".to_string()),
            c if (c as u32) < 0x20 => {
                return Err("nginx config value contains a control character".to_string());
            }
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

pub fn nginx_quote_path(path: &Path) -> Result<String, String> {
    let value = path
        .to_str()
        .ok_or_else(|| "nginx config path is not valid UTF-8".to_string())?;
    Ok(format!("\"{}\"", nginx_escape_quoted(value)?))
}

pub fn render_nginx_conf(template: &str, prefix: &Path, module: &Path) -> Result<String, String> {
    let prefix = prefix
        .to_str()
        .ok_or_else(|| "nginx prefix path is not valid UTF-8".to_string())?;
    let rendered = template
        .replace("@@MODULE@@", &nginx_quote_path(module)?)
        .replace("@@PREFIX@@", &nginx_escape_quoted(prefix)?);
    Ok(rendered)
}

pub fn write_conf(prefix: &Path, module: &Path) -> PathBuf {
    let template = include_str!("../fixtures/nginx.conf.in");
    let rendered = render_nginx_conf(template, prefix, module)
        .unwrap_or_else(|err| panic!("unsafe nginx config path: {err}"));
    let conf = prefix.join("conf/nginx.conf");
    fs::write(&conf, rendered).unwrap();
    conf
}

pub fn nginx_test(nginx: &Path, prefix: &Path) -> std::process::Output {
    Command::new(nginx)
        .arg("-t")
        .arg("-p")
        .arg(prefix)
        .arg("-c")
        .arg("conf/nginx.conf")
        .output()
        .expect("failed to spawn nginx -t")
}

pub fn combined(out: &std::process::Output) -> String {
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&out.stdout));
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text
}

/// True when `nginx -t` failed because `load_module` could not dlopen this exact path.
pub fn is_missing_load_module_error(text: &str, module_path: &Path, conf_path: &Path) -> bool {
    let module = module_path.display().to_string();
    let conf = conf_path.display().to_string();
    text.contains(&format!("dlopen() \"{module}\" failed")) && text.contains(&conf)
}

#[test]
fn missing_load_error_requires_exact_path_and_load_module_context() {
    let missing = Path::new("/prefix/modules/libngx_rustscript-missing.so");
    let conf = Path::new("/prefix/conf/nginx.conf");
    let load_failure = format!(
        "nginx: [emerg] dlopen() \"{}\" failed ({}: cannot open shared object file: No such file or directory) in {}:7",
        missing.display(),
        missing.display(),
        conf.display()
    );
    let unrelated =
        "nginx: [emerg] open() \"/prefix/html/index.html\" failed (No such file or directory)";
    let other_module = format!(
        "nginx: [emerg] dlopen() \"/elsewhere/wrong.so\" failed (/elsewhere/wrong.so: cannot open shared object file: No such file or directory) in {}:7",
        conf.display()
    );
    assert!(is_missing_load_module_error(&load_failure, missing, conf));
    assert!(
        !is_missing_load_module_error(unrelated, missing, conf),
        "a generic No-such-file must not pass"
    );
    assert!(
        !is_missing_load_module_error(&other_module, missing, conf),
        "dlopen of a different path must not pass"
    );
}

#[test]
fn nginx_quote_path_quotes_spaces_and_escapes_quotes() {
    assert_eq!(
        nginx_quote_path(Path::new("/tmp/foo bar/mod.so")).unwrap(),
        "\"/tmp/foo bar/mod.so\""
    );
    assert_eq!(
        nginx_quote_path(Path::new("/tmp/foo\"bar\\baz")).unwrap(),
        "\"/tmp/foo\\\"bar\\\\baz\""
    );
    assert_eq!(
        nginx_quote_path(Path::new("/tmp/foo;bar")).unwrap(),
        "\"/tmp/foo;bar\""
    );
}

#[test]
fn nginx_quote_path_rejects_newline_and_non_utf8() {
    let err = nginx_quote_path(Path::new("/tmp/foo\nbar")).expect_err("newline");
    assert!(err.contains("newline"), "got {err}");
    let non_utf8 = PathBuf::from(OsStr::from_bytes(b"/tmp/foo\xffbar"));
    let err = nginx_quote_path(&non_utf8).expect_err("non-utf8");
    assert!(err.contains("UTF-8"), "got {err}");
}

#[test]
fn render_nginx_conf_quotes_interpolated_paths() {
    let template = include_str!("../fixtures/nginx.conf.in");
    let rendered = render_nginx_conf(
        template,
        Path::new("/tmp/pre fix"),
        Path::new("/tmp/mod ule.so"),
    )
    .unwrap();
    assert!(rendered.contains("pid \"/tmp/pre fix/logs/nginx.pid\";"));
    assert!(rendered.contains("load_module \"/tmp/mod ule.so\";"));
    assert!(!rendered.contains("load_module /tmp/mod ule.so;"));
}

#[test]
fn render_nginx_conf_rejects_newline_quote_injection_and_non_utf8() {
    let template = include_str!("../fixtures/nginx.conf.in");
    let err = render_nginx_conf(template, Path::new("/tmp/foo\nbar"), Path::new("/mod.so"))
        .expect_err("newline prefix");
    assert!(err.contains("newline"), "got {err}");
    let non_utf8 = PathBuf::from(OsStr::from_bytes(b"/tmp/\xffprefix"));
    let err = render_nginx_conf(template, &non_utf8, Path::new("/mod.so")).expect_err("non-utf8");
    assert!(err.contains("UTF-8"), "got {err}");
}
#[test]
fn nginx_v_output_accepts_pinned_openresty_with_required_configure_args() {
    let text = "\
nginx version: openresty/1.31.4.1
built by gcc 13.3.0 (Ubuntu 13.3.0-6ubuntu2~24.04.1)
configure arguments: --prefix=/tmp/nginx-prefix --with-compat --with-http_ssl_module
";
    let report = parse_nginx_v_output(text).expect("parse pinned nginx -V");
    validate_nginx_version_report(&report).expect("pinned ABI");
}

#[test]
fn nginx_v_output_rejects_wrong_version() {
    let text = "\
nginx version: nginx/1.24.0
configure arguments: --prefix=/tmp/nginx-prefix --with-compat --with-http_ssl_module
";
    let report = parse_nginx_v_output(text).expect("parse wrong version");
    let err = validate_nginx_version_report(&report).expect_err("1.24.0 must not pass");
    assert!(
        err.contains("1.31.4.1") && err.contains("1.24.0"),
        "error should name pinned vs actual version, got {err}"
    );
}

#[test]
fn nginx_v_output_rejects_wrong_configure_args() {
    let text = "\
nginx version: openresty/1.31.4.1
configure arguments: --prefix=/tmp/nginx-prefix --with-compat
";
    let report = parse_nginx_v_output(text).expect("parse missing ssl");
    let err = validate_nginx_version_report(&report).expect_err("missing ssl module must not pass");
    assert!(
        err.contains("--with-http_ssl_module") || err.contains("configure"),
        "error should name missing configure arg, got {err}"
    );
}

#[test]
fn nginx_v_output_accepts_explicit_path_shaped_builddir() {
    let text = "\
nginx version: openresty/1.31.4.1
configure arguments: --prefix=/tmp/nginx-prefix --builddir=/tmp/custom-objs --with-compat --with-http_ssl_module
";
    let report = parse_nginx_v_output(text).expect("parse custom builddir");
    validate_nginx_version_report(&report).expect("official --builddir must be accepted");
}

#[test]
fn nginx_v_output_accepts_relative_path_shaped_builddir() {
    let text = "\
nginx version: openresty/1.31.4.1
configure arguments: --prefix=/tmp/nginx-prefix --builddir=objs-custom --with-compat --with-http_ssl_module
";
    let report = parse_nginx_v_output(text).expect("parse relative builddir");
    validate_nginx_version_report(&report).expect("relative --builddir must be accepted");
}

#[test]
fn nginx_v_output_rejects_empty_or_flag_shaped_builddir() {
    let empty = "\
nginx version: openresty/1.31.4.1
configure arguments: --prefix=/tmp/nginx-prefix --builddir= --with-compat --with-http_ssl_module
";
    let report = parse_nginx_v_output(empty).expect("parse empty builddir");
    let err = validate_nginx_version_report(&report).expect_err("empty --builddir must not pass");
    assert!(
        err.contains("--builddir"),
        "error should name --builddir, got {err}"
    );

    let flag_shaped = "\
nginx version: openresty/1.31.4.1
configure arguments: --prefix=/tmp/nginx-prefix --builddir=--with-evil --with-compat --with-http_ssl_module
";
    let report = parse_nginx_v_output(flag_shaped).expect("parse flag-shaped builddir");
    let err =
        validate_nginx_version_report(&report).expect_err("flag-shaped --builddir must not pass");
    assert!(
        err.contains("--builddir"),
        "error should name --builddir, got {err}"
    );
}

#[test]
fn nginx_v_output_accepts_quoted_prefix_with_spaces() {
    let text = "\
nginx version: openresty/1.31.4.1
configure arguments: --prefix='/tmp/pre fix' --with-compat --with-http_ssl_module
";
    let report = parse_nginx_v_output(text).expect("parse quoted prefix");
    assert_eq!(
        report.configure_args,
        vec![
            "--prefix=/tmp/pre fix".to_string(),
            "--with-compat".to_string(),
            "--with-http_ssl_module".to_string(),
        ]
    );
    validate_nginx_version_report(&report).expect("quoted prefix must be accepted");
}

#[test]
fn nginx_v_output_accepts_add_dynamic_module_and_other_valid_flags() {
    let text = "\
nginx version: openresty/1.31.4.1
configure arguments: --prefix=/tmp/nginx-prefix --with-compat --with-http_ssl_module --builddir=objs-custom --add-dynamic-module=/tmp/ngx-rustscript --with-http_v2
";
    let report = parse_nginx_v_output(text).expect("parse add-dynamic");
    validate_nginx_version_report(&report)
        .expect("--add-dynamic-module and other valid NGINX flags must be accepted");
}

#[test]
fn nginx_v_output_rejects_malformed_quoting_and_control_bytes() {
    let unclosed = "\
nginx version: openresty/1.31.4.1
configure arguments: --prefix='/tmp/pre fix --with-compat --with-http_ssl_module
";
    let err = parse_nginx_v_output(unclosed).expect_err("unclosed quote");
    assert!(
        err.contains("quot") || err.contains("malformed"),
        "error should name malformed quoting, got {err}"
    );

    let control = "\
nginx version: openresty/1.31.4.1
configure arguments: --prefix=/tmp/foo\u{01}bar --with-compat --with-http_ssl_module
";
    let err = parse_nginx_v_output(control).expect_err("control byte");
    assert!(
        err.contains("control") || err.contains("NUL"),
        "error should name control/NUL, got {err}"
    );
}

#[test]
fn nginx_binary_accepts_explicit_openresty_install() {
    let prefix = isolated_prefix("nginx-canon");
    let build_dir = prefix.join("objs");
    fs::create_dir_all(&build_dir).unwrap();
    let nginx = build_dir.join("nginx");
    fs::write(&nginx, b"#!/bin/true\n").unwrap();
    let other = prefix.join("other-nginx");
    fs::write(&other, b"#!/bin/true\n").unwrap();
    let canonical = fs::canonicalize(&nginx).unwrap();
    assert_eq!(
        resolve_nginx_binary(&build_dir, None).expect("canonical nginx"),
        canonical
    );
    let err = resolve_nginx_binary(&build_dir, Some(&other))
        .expect("an explicit installed OpenResty binary is selected by the harness");
    assert_eq!(err, fs::canonicalize(&other).unwrap());

    let missing_build = prefix.join("missing-objs");
    let err = resolve_nginx_binary(&missing_build, Some(&other))
        .expect_err("generated-header build directory is mandatory");
    assert!(
        err.contains("NGINX_BUILD_DIR"),
        "error should require the generated-header directory, got {err}"
    );
}
