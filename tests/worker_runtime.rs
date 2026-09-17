//! Worker init/exit lifecycle: runtime after fork, not during `nginx -t`.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use support::artifact::module_cdylib;
use support::nginx::{
    combined, nginx_binary, nginx_process_group_command, nginx_quote_path, nginx_test,
    terminate_process_group,
};
use support::temp::isolated_prefix;

struct RunningNginx {
    child: Child,
}

impl Drop for RunningNginx {
    fn drop(&mut self) {
        terminate_process_group(&mut self.child, Duration::from_secs(2));
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn write_conf(prefix: &Path, body: &str) {
    fs::write(prefix.join("conf/nginx.conf"), body).unwrap();
}

fn conf_body(module: &Path, prefix: &Path, port: u16, error_log_level: &str) -> String {
    let module = nginx_quote_path(module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    let error_log = nginx_quote_path(&prefix.join("logs/error.log")).unwrap();
    format!(
        r#"
worker_processes 1;
error_log {error_log} {error_log_level};
pid {pid};
daemon off;
master_process on;
load_module {module};

events {{
    worker_connections 32;
}}

http {{
    access_log off;
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
        location / {{
            return 200 "ok\n";
        }}
    }}
}}
"#
    )
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

fn http_get(port: u16, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
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

fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if pred() {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn error_log(prefix: &Path) -> String {
    fs::read_to_string(prefix.join("logs/error.log")).unwrap_or_default()
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

#[test]
fn nginx_t_does_not_start_worker_runtime() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("t-runtime-test");
    let port = free_port();
    write_conf(&prefix, &conf_body(&module, &prefix, port, "notice"));
    let start = Instant::now();
    let out = nginx_test(&nginx, &prefix);
    let elapsed = start.elapsed();
    let text = combined(&out);
    let log = error_log(&prefix);
    assert!(
        out.status.success(),
        "nginx -t must succeed:\n{text}\n{log}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "nginx -t hung, suggesting a leaked Tokio runtime: {elapsed:?}"
    );
    assert!(
        !text.contains("worker runtime ready") && !log.contains("worker runtime ready"),
        "nginx -t must not run init_process / create runtime threads or eventfd:\n{text}\n{log}"
    );
}

#[test]
fn worker_init_serves_http_and_exits_cleanly() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("t-runtime-worker");
    let port = free_port();
    write_conf(&prefix, &conf_body(&module, &prefix, port, "notice"));
    let mut running = spawn_nginx(&nginx, &prefix);
    wait_for_port(port, Duration::from_secs(5));
    assert!(
        wait_until(Duration::from_secs(2), || {
            error_log(&prefix).contains("ngx-rustscript: worker runtime ready")
        }),
        "worker init_process must start the runtime, log was:\n{}",
        error_log(&prefix)
    );
    let (status, body) = http_get(port, "/");
    assert_eq!(status, 200, "ordinary HTTP must stay responsive:\n{body}");
    quit_and_wait(&mut running.child, Duration::from_secs(8));
    let final_log = error_log(&prefix);
    assert!(
        !final_log.to_lowercase().contains("segfault")
            && !final_log.to_lowercase().contains("abort"),
        "worker exit must be clean:\n{final_log}"
    );
}
