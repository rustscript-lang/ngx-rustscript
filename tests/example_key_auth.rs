//! Compile, HMAC, and pinned-NGINX checks for `examples/key-auth.*`.

use ngx_rustscript_nginx_test_stubs as _;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use ngx_rustscript::config::compile_rss_source;
use ngx_rustscript::host::{bind_ngx_hosts, production_runtime_host_names};
use sha1::Sha1;
use vm::{Value, Vm, VmStatus};

mod support;
use support::artifact::module_cdylib;
use support::nginx::{
    combined, nginx_binary, nginx_process_group_command, nginx_quote_path, nginx_test,
    terminate_process_group,
};
use support::temp::isolated_prefix;

const EXAMPLE_RSS: &str = include_str!("../examples/key-auth.rss");
const EXAMPLE_CONF: &str = include_str!("../examples/key-auth.nginx.conf");
const EXAMPLE_SECRETS: &str = include_str!("../examples/key-auth-secrets.conf.example");
const EXAMPLE_DOC: &str = include_str!("../docs/examples/key-auth.md");

const PEPPER: &str = "test-pepper-not-for-production-use-0001";
const TOKEN_CURRENT: &str = "tok_test_alpha_high_entropy_0001";
const TOKEN_PREVIOUS: &str = "tok_test_beta_high_entropy_0002";
const TOKEN_WRONG: &str = "tok_test_wrong_high_entropy_0003";
const TOKEN_NEXT: &str = "tok_test_gamma_high_entropy_0004";

const PLACEHOLDER_PEPPER: &str = "REPLACE_WITH_HIGH_ENTROPY_PEPPER";
const PLACEHOLDER_DIGEST: &str = "REPLACE_WITH_BASE64_NOPAD_HMAC_SHA1_DIGEST";

const WWW_AUTHENTICATE: &str = "ApiKey realm=\"api\"";
const UPSTREAM_BODY: &str = "ok";

fn example_rss_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/key-auth.rss")
}

fn rust_hmac_sha1_b64_nopad(pepper: &str, token: &str) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(pepper.as_bytes()).expect("hmac key");
    mac.update(token.as_bytes());
    let bytes = mac.finalize().into_bytes();
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
        .trim_end_matches('=')
        .to_string()
}

fn python_hmac_sha1_b64_nopad(pepper: &str, token: &str) -> String {
    let output = Command::new("python3")
        .args([
            "-c",
            "import base64,hashlib,hmac,sys\n\
             digest=hmac.new(sys.argv[1].encode(), sys.argv[2].encode(), hashlib.sha1).digest()\n\
             sys.stdout.write(base64.b64encode(digest).rstrip(b'=').decode())\n",
            pepper,
            token,
        ])
        .output()
        .expect("python3 hmac");
    assert!(
        output.status.success(),
        "python hmac failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("python hmac utf-8")
}

fn ngx_hmac_sha1_b64_nopad(pepper: &str, token: &str) -> String {
    let pepper = pepper.replace('\\', "\\\\").replace('"', "\\\"");
    let token = token.replace('\\', "\\\\").replace('"', "\\\"");
    let source =
        format!("use ngx;\nngx::encode_base64(ngx::hmac_sha1(\"{pepper}\", \"{token}\"), true);\n");
    let program = compile_rss_source(&source).unwrap_or_else(|err| panic!("compile hmac: {err}"));
    let mut vm = Vm::try_new((*program).clone()).expect("vm");
    bind_ngx_hosts(&mut vm);
    let status = vm.run().unwrap_or_else(|err| panic!("run hmac: {err}"));
    assert_eq!(status, VmStatus::Halted);
    match vm.stack().last() {
        Some(Value::String(text)) => (**text).clone(),
        other => panic!("expected digest string, got {other:?}"),
    }
}

fn secrets() -> [&'static str; 5] {
    [
        PEPPER,
        TOKEN_CURRENT,
        TOKEN_PREVIOUS,
        TOKEN_WRONG,
        TOKEN_NEXT,
    ]
}

fn live_secrets(digests: &[&str]) -> Vec<String> {
    let mut out = vec![
        PEPPER.to_string(),
        TOKEN_CURRENT.to_string(),
        TOKEN_PREVIOUS.to_string(),
        TOKEN_WRONG.to_string(),
        TOKEN_NEXT.to_string(),
    ];
    for digest in digests {
        if !digest.is_empty() {
            out.push((*digest).to_string());
        }
    }
    out
}

fn assert_no_secrets(label: &str, text: &str, secrets: &[String]) {
    for secret in secrets {
        if secret.is_empty() {
            continue;
        }
        assert!(
            !text.contains(secret),
            "{label} leaked secret {secret:?}: {text}"
        );
    }
}

fn header_value<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    let headers = response.split("\r\n\r\n").next().unwrap_or(response);
    for line in headers.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case(name) {
                return Some(value.trim());
            }
        }
    }
    None
}

fn header_present(block: &str, name: &str) -> bool {
    for line in block.split("\r\n") {
        if let Some((key, _)) = line.split_once(':') {
            if key.eq_ignore_ascii_case(name) {
                return true;
            }
        }
    }
    false
}

fn response_body(response: &str) -> &str {
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.trim_end())
        .unwrap_or("")
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct RunningNginx {
    child: Child,
}

impl Drop for RunningNginx {
    fn drop(&mut self) {
        terminate_process_group(&mut self.child, Duration::from_secs(2));
    }
}

fn spawn_nginx(nginx: &Path, prefix: &Path) -> RunningNginx {
    let child = nginx_process_group_command(nginx)
        .arg("-p")
        .arg(prefix)
        .arg("-c")
        .arg("conf/nginx.conf")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn nginx");
    RunningNginx { child }
}

fn wait_for_port(port: u16, timeout: Duration) {
    support::nginx::wait_for_worker_http(port, timeout);
}

fn http_exchange(port: u16, request: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let _ = stream.flush();
    let mut body = String::new();
    stream.read_to_string(&mut body).unwrap();
    let status = body
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, body)
}

fn get_with_named_api_keys(
    port: u16,
    path: &str,
    header_name: &str,
    keys: &[&str],
) -> (u16, String) {
    let mut request = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    for key in keys {
        request.push_str(header_name);
        request.push_str(": ");
        request.push_str(key);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    http_exchange(port, &request)
}

fn get_with_api_keys(port: u16, path: &str, keys: &[&str]) -> (u16, String) {
    get_with_named_api_keys(port, path, "X-API-Key", keys)
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

struct Upstream {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
}

fn start_upstream() -> Upstream {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    std::thread::spawn(move || {
        listener.set_nonblocking(true).ok();
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
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
                    captured
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&buf).into_owned());
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    );
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    Upstream { port, requests }
}

fn secrets_conf(pepper: &str, digest: &str, previous: &str) -> String {
    format!(
        "set $rss_api_key_pepper \"{pepper}\";\n\
         set $rss_api_key_digest \"{digest}\";\n\
         set $rss_api_key_digest_prev \"{previous}\";\n"
    )
}

fn write_secrets_include(path: &Path, contents: &str) {
    let tmp = path.with_file_name("key-auth-secrets.conf.tmp");
    fs::write(&tmp, contents).unwrap();
    let mut perms = fs::metadata(&tmp).unwrap().permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&tmp, perms).unwrap();
    fs::rename(&tmp, path).unwrap();
    let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "secrets include must be mode 0600, got {mode:#o}"
    );
}

fn render_example_conf(
    module: &Path,
    script: &Path,
    listen: u16,
    upstream: u16,
    secrets_include: &Path,
) -> String {
    EXAMPLE_CONF
        .replace(
            "REPLACE_WITH_LIBNGX_RUSTSCRIPT_SO",
            &nginx_quote_path(module).unwrap(),
        )
        .replace(
            "REPLACE_WITH_KEY_AUTH_RSS",
            &nginx_quote_path(script).unwrap(),
        )
        .replace("REPLACE_WITH_LISTEN", &format!("127.0.0.1:{listen}"))
        .replace(
            "REPLACE_WITH_UPSTREAM",
            &format!("http://127.0.0.1:{upstream}"),
        )
        .replace(
            "REPLACE_WITH_SECRETS_INCLUDE",
            &nginx_quote_path(secrets_include).unwrap(),
        )
}

fn write_prefix_example(prefix: &Path, conf: &str, script: &str) -> PathBuf {
    let script_path = prefix.join("conf/key-auth.rss");
    fs::write(&script_path, script).unwrap();
    fs::write(prefix.join("conf/nginx.conf"), conf).unwrap();
    script_path
}

fn nginx_worker_pids(master_pid: u32) -> Vec<u32> {
    let children = format!("/proc/{master_pid}/task/{master_pid}/children");
    fs::read_to_string(&children)
        .unwrap_or_else(|err| panic!("read {children}: {err}"))
        .split_whitespace()
        .map(|pid| {
            pid.parse::<u32>()
                .unwrap_or_else(|err| panic!("parse nginx worker pid {pid}: {err}"))
        })
        .collect()
}

fn reload_nginx(nginx: &Path, prefix: &Path, master_pid: u32) {
    let old_workers = nginx_worker_pids(master_pid);
    assert!(
        !old_workers.is_empty(),
        "nginx master has no workers before reload"
    );
    let out = nginx_test(nginx, prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "nginx -t before reload failed:\n{text}"
    );
    let reload = Command::new(nginx)
        .arg("-p")
        .arg(prefix)
        .arg("-c")
        .arg("conf/nginx.conf")
        .arg("-s")
        .arg("reload")
        .output()
        .expect("reload");
    assert!(
        reload.status.success(),
        "reload failed: {}",
        combined(&reload)
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let current = nginx_worker_pids(master_pid);
        if !current.is_empty() && old_workers.iter().all(|pid| !current.contains(pid)) {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "old nginx workers {old_workers:?} did not retire within 5s; current={current:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_upstream_stripped(raw: &str, secrets: &[String]) {
    assert!(
        !header_present(raw, "X-API-Key"),
        "upstream must not receive X-API-Key: {raw}"
    );
    assert!(
        !header_present(raw, "X_API_Key"),
        "upstream must not receive X_API_Key: {raw}"
    );
    assert_no_secrets("upstream request", raw, secrets);
}

fn assert_ok_200(label: &str, status: u16, response: &str, secrets: &[String]) {
    assert_eq!(status, 200, "{label}: {response}");
    assert_eq!(
        response_body(response),
        UPSTREAM_BODY,
        "{label} upstream body: {response}"
    );
    assert_no_secrets(label, response, secrets);
}

#[test]
fn production_host_count_stays_199() {
    assert_eq!(production_runtime_host_names().len(), 199);
}

#[test]
fn example_script_compiles_for_access_phase() {
    compile_rss_source(EXAMPLE_RSS).unwrap_or_else(|err| panic!("example rss must compile: {err}"));
}

#[test]
fn example_files_are_copy_paste_safe() {
    for text in [EXAMPLE_CONF, EXAMPLE_SECRETS, EXAMPLE_DOC] {
        assert!(
            text.contains("WARNING"),
            "copy-paste material must include uppercase WARNING"
        );
        assert!(
            !text.contains(PEPPER),
            "example material must not contain live test pepper"
        );
    }
    assert!(EXAMPLE_CONF.contains("REPLACE_"));
    assert!(EXAMPLE_SECRETS.contains(PLACEHOLDER_PEPPER));
    assert!(EXAMPLE_SECRETS.contains(PLACEHOLDER_DIGEST));
    assert!(
        EXAMPLE_SECRETS.contains("set $rss_api_key_digest_prev \"\";"),
        "template previous digest must default to the empty string"
    );
    assert!(
        !EXAMPLE_SECRETS.contains("REPLACE_WITH_PREVIOUS"),
        "previous digest must not be a replaceable authorizing placeholder"
    );
    assert!(EXAMPLE_CONF.contains("include REPLACE_WITH_SECRETS_INCLUDE;"));
    assert!(EXAMPLE_CONF.contains("underscores_in_headers off;"));
    assert!(EXAMPLE_CONF.contains("proxy_set_header X-API-Key \"\";"));
    assert!(EXAMPLE_CONF.contains("proxy_set_header X_API_Key \"\";"));
    assert!(!EXAMPLE_CONF.contains("set $rss_api_key_pepper"));
    assert!(EXAMPLE_DOC.contains("HMAC-SHA1"));
    assert!(EXAMPLE_DOC.contains("not a message signature"));
    assert!(EXAMPLE_DOC.contains("0600"));
    assert!(
        EXAMPLE_DOC.contains("not constant-time"),
        "docs must deny constant-time compare"
    );
    assert!(
        !EXAMPLE_DOC.contains("is constant-time"),
        "docs must not claim constant-time compare"
    );
    assert!(
        EXAMPLE_DOC.contains("$http_x_api_key"),
        "docs must name the header log vector"
    );
    assert!(
        EXAMPLE_DOC.contains("$request"),
        "docs must name $request as a second independent log vector"
    );
    assert!(EXAMPLE_DOC.contains("Query-string tokens are unsupported"));
    assert!(EXAMPLE_DOC.contains("atomically"));
    assert!(EXAMPLE_DOC.contains("underscores_in_headers on"));
    for secret in secrets() {
        assert!(
            !EXAMPLE_CONF.contains(secret),
            "example conf must not contain live test secret {secret}"
        );
        assert!(
            !EXAMPLE_RSS.contains(secret),
            "example rss must not contain live test secret {secret}"
        );
        assert!(
            !EXAMPLE_SECRETS.contains(secret),
            "example secrets must not contain live test secret {secret}"
        );
    }
    assert_ne!(
        rust_hmac_sha1_b64_nopad(PLACEHOLDER_PEPPER, TOKEN_CURRENT),
        PLACEHOLDER_DIGEST,
        "placeholder pepper/digest must not authorize TOKEN_CURRENT"
    );
    assert_ne!(
        rust_hmac_sha1_b64_nopad(PLACEHOLDER_PEPPER, TOKEN_PREVIOUS),
        PLACEHOLDER_DIGEST,
        "placeholder pepper/digest must not authorize TOKEN_PREVIOUS"
    );
    assert_ne!(
        rust_hmac_sha1_b64_nopad(PLACEHOLDER_PEPPER, TOKEN_NEXT),
        PLACEHOLDER_DIGEST,
        "placeholder pepper/digest must not authorize TOKEN_NEXT"
    );
    assert!(
        EXAMPLE_RSS.contains("api key auth is not configured"),
        "missing-config log must be a constant string"
    );
    assert!(
        EXAMPLE_RSS.contains("http_x_api_key"),
        "script must read X-API-Key via ngx::var::get(\"http_x_api_key\")"
    );
    assert!(
        EXAMPLE_RSS.contains("not constant-time"),
        "script comments must keep honest non-constant =="
    );
}

#[test]
fn hmac_sha1_nopad_digest_matches_rust_and_python() {
    let rust = rust_hmac_sha1_b64_nopad(PEPPER, TOKEN_CURRENT);
    let python = python_hmac_sha1_b64_nopad(PEPPER, TOKEN_CURRENT);
    let ngx = ngx_hmac_sha1_b64_nopad(PEPPER, TOKEN_CURRENT);
    assert_eq!(rust, python, "Rust and Python digests must match");
    assert_eq!(rust, ngx, "independent digest must match ngx::hmac_sha1");
    assert!(
        !rust.contains('='),
        "digest must be unpadded Base64: {rust}"
    );
    assert_ne!(
        rust,
        rust_hmac_sha1_b64_nopad(PEPPER, TOKEN_PREVIOUS),
        "distinct tokens must not share a digest"
    );
    assert_ne!(
        rust,
        rust_hmac_sha1_b64_nopad(PEPPER, TOKEN_NEXT),
        "rotation next token must not share a digest"
    );
}

fn run_pinned_example_once() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("example-key-auth");
    let listen = free_port();
    let upstream = start_upstream();
    let digest_a = rust_hmac_sha1_b64_nopad(PEPPER, TOKEN_CURRENT);
    let digest_b = rust_hmac_sha1_b64_nopad(PEPPER, TOKEN_PREVIOUS);
    let digest_c = rust_hmac_sha1_b64_nopad(PEPPER, TOKEN_NEXT);
    assert_eq!(digest_a, python_hmac_sha1_b64_nopad(PEPPER, TOKEN_CURRENT));
    let script_path = prefix.join("conf/key-auth.rss");
    fs::write(&script_path, EXAMPLE_RSS).unwrap();
    let secrets_path = prefix.join("conf/key-auth-secrets.conf");
    write_secrets_include(&secrets_path, &secrets_conf(PEPPER, &digest_a, &digest_b));
    let conf = render_example_conf(&module, &script_path, listen, upstream.port, &secrets_path);
    fs::write(prefix.join("conf/nginx.conf"), &conf).unwrap();
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "example conf nginx -t failed:\n{text}"
    );

    let mut running = spawn_nginx(&nginx, &prefix);
    wait_for_port(listen, Duration::from_secs(5));
    let secret_list = live_secrets(&[&digest_a, &digest_b, &digest_c]);

    let (status, response) = get_with_api_keys(listen, "/", &[]);
    assert_eq!(status, 401, "missing header: {response}");
    assert_eq!(
        header_value(&response, "WWW-Authenticate"),
        Some(WWW_AUTHENTICATE),
        "401 must set WWW-Authenticate: {response}"
    );
    assert_no_secrets("missing-header response", &response, &secret_list);

    let (status, response) = get_with_api_keys(listen, "/?api_key=not-a-secret", &[]);
    assert_eq!(status, 401, "query token is unsupported: {response}");
    assert_eq!(
        header_value(&response, "WWW-Authenticate"),
        Some(WWW_AUTHENTICATE),
        "query-only request is a missing header: {response}"
    );

    let (status, response) = get_with_api_keys(listen, "/", &[""]);
    assert_eq!(status, 403, "empty header: {response}");
    assert!(
        header_value(&response, "WWW-Authenticate").is_none(),
        "403 must not send WWW-Authenticate: {response}"
    );
    assert_no_secrets("empty-header response", &response, &secret_list);

    let (status, response) = get_with_api_keys(listen, "/", &[TOKEN_WRONG]);
    assert_eq!(status, 403, "wrong token: {response}");
    assert_no_secrets("wrong-token response", &response, &secret_list);

    let (status, response) = get_with_api_keys(listen, "/", &[TOKEN_CURRENT, TOKEN_PREVIOUS]);
    assert_eq!(status, 403, "duplicate headers: {response}");
    assert_no_secrets("duplicate-header response", &response, &secret_list);
    assert!(
        upstream.requests.lock().unwrap().is_empty(),
        "rejected requests must not reach upstream"
    );

    let (status, response) = get_with_api_keys(listen, "/", &[TOKEN_CURRENT]);
    assert_ok_200("valid current A", status, &response, &secret_list);

    let (status, response) = get_with_api_keys(listen, "/", &[TOKEN_PREVIOUS]);
    assert_ok_200("valid previous B", status, &response, &secret_list);

    let (status, response) = get_with_named_api_keys(listen, "/", "x-api-key", &[TOKEN_CURRENT]);
    assert_ok_200("mixed-case inbound header", status, &response, &secret_list);

    {
        let captured = upstream.requests.lock().unwrap();
        assert_eq!(
            captured.len(),
            3,
            "A, B, and mixed-case must reach upstream"
        );
        for raw in captured.iter() {
            assert_upstream_stripped(raw, &secret_list);
        }
    }

    let mut handles = Vec::new();
    for i in 0..6 {
        handles.push(std::thread::spawn(move || match i % 3 {
            0 => get_with_api_keys(listen, "/", &[TOKEN_CURRENT]),
            1 => get_with_named_api_keys(listen, "/", "X-Api-Key", &[TOKEN_CURRENT]),
            _ => get_with_api_keys(listen, "/", &[TOKEN_CURRENT, TOKEN_PREVIOUS]),
        }));
    }
    let mut seen_ok = 0;
    let mut seen_forbidden = 0;
    for handle in handles {
        let (status, response) = handle.join().unwrap();
        match status {
            200 => {
                seen_ok += 1;
                assert_eq!(response_body(&response), UPSTREAM_BODY);
            }
            403 => seen_forbidden += 1,
            other => panic!("concurrent isolation got {other}: {response}"),
        }
        assert_no_secrets("concurrent response", &response, &secret_list);
    }
    assert_eq!(seen_ok, 4, "concurrent valid requests including mixed-case");
    assert_eq!(seen_forbidden, 2, "concurrent repeated-header rejections");

    {
        let captured = upstream.requests.lock().unwrap();
        assert_eq!(
            captured.len(),
            7,
            "every concurrent 200 must reach upstream, got {}",
            captured.len()
        );
        for raw in captured.iter() {
            assert_upstream_stripped(raw, &secret_list);
        }
    }

    write_secrets_include(&secrets_path, &secrets_conf(PEPPER, &digest_c, &digest_a));
    reload_nginx(&nginx, &prefix, running.child.id());

    let (status, response) = get_with_api_keys(listen, "/", &[TOKEN_NEXT]);
    assert_ok_200("rotation current C", status, &response, &secret_list);
    let (status, response) = get_with_api_keys(listen, "/", &[TOKEN_CURRENT]);
    assert_ok_200("rotation previous A", status, &response, &secret_list);
    let (status, response) = get_with_api_keys(listen, "/", &[TOKEN_PREVIOUS]);
    assert_eq!(status, 403, "retired B after rotation: {response}");
    assert!(
        header_value(&response, "WWW-Authenticate").is_none(),
        "403 must not send WWW-Authenticate: {response}"
    );

    write_secrets_include(&secrets_path, &secrets_conf(PEPPER, &digest_c, ""));
    reload_nginx(&nginx, &prefix, running.child.id());

    let (status, response) = get_with_api_keys(listen, "/", &[TOKEN_NEXT]);
    assert_ok_200(
        "current C after prev cleared",
        status,
        &response,
        &secret_list,
    );
    let (status, response) = get_with_api_keys(listen, "/", &[TOKEN_CURRENT]);
    assert_eq!(status, 403, "retired A after prev cleared: {response}");

    {
        let captured = upstream.requests.lock().unwrap();
        for raw in captured.iter() {
            assert_upstream_stripped(raw, &secret_list);
        }
    }

    let error_log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    let access_log = fs::read_to_string(prefix.join("logs/access.log")).unwrap_or_default();
    assert_no_secrets("error.log", &error_log, &secret_list);
    assert_no_secrets("access.log", &access_log, &secret_list);

    quit_and_wait(&mut running.child, Duration::from_secs(8));
}

#[test]
fn pinned_nginx_key_auth_statuses_stripping_isolation_reload_sigquit() {
    run_pinned_example_once();
    run_pinned_example_once();
}

fn run_fail_closed(pepper: &str, digest: &str, label: &str) {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix(&format!("example-key-auth-{label}"));
    let listen = free_port();
    let upstream = start_upstream();
    let digest_current = rust_hmac_sha1_b64_nopad(PEPPER, TOKEN_CURRENT);
    let script_path = prefix.join("conf/key-auth.rss");
    fs::write(&script_path, EXAMPLE_RSS).unwrap();
    let secrets_path = prefix.join("conf/key-auth-secrets.conf");
    write_secrets_include(&secrets_path, &secrets_conf(pepper, digest, ""));
    let conf = render_example_conf(&module, &script_path, listen, upstream.port, &secrets_path);
    fs::write(prefix.join("conf/nginx.conf"), conf).unwrap();
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "{label} conf nginx -t failed:\n{text}"
    );
    let _running = spawn_nginx(&nginx, &prefix);
    wait_for_port(listen, Duration::from_secs(5));
    let secret_list = live_secrets(&[&digest_current]);
    let (status, response) = get_with_api_keys(listen, "/", &[TOKEN_CURRENT]);
    assert_eq!(status, 500, "{label} must fail closed: {response}");
    assert!(
        header_value(&response, "WWW-Authenticate").is_none(),
        "{label} 500 must not send WWW-Authenticate: {response}"
    );
    assert_no_secrets(&format!("{label} response"), &response, &secret_list);
    let error_log = fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default();
    assert!(
        error_log.contains("api key auth is not configured"),
        "{label} must log a constant message: {error_log}"
    );
    assert_no_secrets(&format!("{label} error.log"), &error_log, &secret_list);
    assert!(
        upstream.requests.lock().unwrap().is_empty(),
        "{label} fail-closed must not reach upstream"
    );
}

#[test]
fn pinned_nginx_missing_pepper_or_digest_fails_closed() {
    let digest_current = rust_hmac_sha1_b64_nopad(PEPPER, TOKEN_CURRENT);
    run_fail_closed("", &digest_current, "empty-pepper");
}

#[test]
fn pinned_nginx_empty_digest_with_pepper_fails_closed() {
    run_fail_closed(PEPPER, "", "empty-digest");
}

#[test]
fn example_conf_nginx_t_accepts_placeholders_after_render() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("example-key-auth-t");
    let listen = free_port();
    let upstream = free_port();
    let digest = rust_hmac_sha1_b64_nopad(PEPPER, TOKEN_CURRENT);
    let secrets = EXAMPLE_SECRETS
        .replace(PLACEHOLDER_PEPPER, PEPPER)
        .replace(PLACEHOLDER_DIGEST, &digest);
    assert!(
        secrets.contains("set $rss_api_key_digest_prev \"\";"),
        "generated include must keep empty previous digest"
    );
    let secrets_path = prefix.join("conf/key-auth-secrets.conf");
    write_secrets_include(&secrets_path, &secrets);
    let script_path = write_prefix_example(
        &prefix,
        &render_example_conf(
            &module,
            &example_rss_path(),
            listen,
            upstream,
            &secrets_path,
        ),
        EXAMPLE_RSS,
    );
    let _ = script_path;
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "rendered example conf must pass nginx -t:\n{text}"
    );
    assert!(text.contains("test is successful"), "{text}");
    let mode = fs::metadata(&secrets_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "generated include must stay 0600");
}
