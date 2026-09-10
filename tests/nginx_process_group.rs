mod support;

use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use support::nginx::{configure_process_group, terminate_process_group};
use support::temp::unique_temp_dir;

fn process_exists(pid: i32) -> bool {
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn wait_for_file(path: &std::path::Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {}", path.display());
}

#[test]
fn process_group_cleanup_terminates_leader_and_descendant() {
    let temp = unique_temp_dir("nginx-process-group");
    let worker_pid_path = temp.join("worker.pid");
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("sleep 60 & printf '%s' \"$!\" > \"$WORKER_PID\"; wait")
        .env("WORKER_PID", &worker_pid_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_process_group(&mut command);
    let mut leader = command.spawn().expect("spawn process-group leader");
    wait_for_file(&worker_pid_path, Duration::from_secs(2));
    let worker_pid: i32 = fs::read_to_string(&worker_pid_path)
        .expect("read worker pid")
        .parse()
        .expect("parse worker pid");
    assert!(process_exists(leader.id() as i32));
    assert!(process_exists(worker_pid));

    terminate_process_group(&mut leader, Duration::from_secs(2));

    assert!(!process_exists(leader.id() as i32));
    let deadline = Instant::now() + Duration::from_secs(2);
    while process_exists(worker_pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !process_exists(worker_pid),
        "descendant {worker_pid} survived"
    );
}
