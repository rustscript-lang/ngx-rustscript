#!/usr/bin/env python3
"""Standalone asynchronous NGINX acceptance for ngx-rustscript.

python3 tests/nginx_integration.py --nginx <absolute pinned nginx> --module <absolute actual cdylib>
"""

from __future__ import annotations

import argparse
import errno
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time

PINNED_VERSION = "nginx/1.30.4"
REQUIRED_CONFIGURE_ARGS = ("--with-compat", "--with-http_ssl_module")
HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")


def task_tmp_root():
    override = os.environ.get("NGX_RUSTSCRIPT_TASK_TMP", "").strip()
    if override:
        return override
    return tempfile.gettempdir()


class CheckError(Exception):
    def __init__(self, message, detail=""):
        super().__init__(message)
        self.detail = detail


def quote_nginx_path(path):
    if "\x00" in path or "\n" in path or "\r" in path:
        raise CheckError("nginx config path contains a control character")
    escaped = path.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def split_shell_words(text):
    if "\x00" in text:
        raise CheckError("configure arguments contain NUL")
    words = []
    cur = []
    in_single = False
    in_double = False
    saw = False
    i = 0
    while i < len(text):
        c = text[i]
        if ord(c) < 0x20 and c != "\t":
            raise CheckError("configure arguments contain a control character")
        if in_single:
            if c == "'":
                in_single = False
            else:
                cur.append(c)
            i += 1
            continue
        if in_double:
            if c == '"':
                in_double = False
            elif c == "\\":
                i += 1
                if i >= len(text):
                    raise CheckError("configure arguments have a dangling backslash")
                n = text[i]
                if n in '"\\$`':
                    cur.append(n)
                elif n != "\n":
                    cur.append("\\")
                    cur.append(n)
            else:
                cur.append(c)
            i += 1
            continue
        if c == "'":
            in_single = True
            saw = True
        elif c == '"':
            in_double = True
            saw = True
        elif c == "\\":
            i += 1
            if i >= len(text):
                raise CheckError("configure arguments have a dangling backslash")
            n = text[i]
            if n != "\n":
                cur.append(n)
                saw = True
        elif c.isspace():
            if saw:
                words.append("".join(cur))
                cur = []
                saw = False
        else:
            cur.append(c)
            saw = True
        i += 1
    if in_single or in_double:
        raise CheckError("configure arguments have malformed quoting")
    if saw:
        words.append("".join(cur))
    return words


def parse_nginx_v(text):
    version = None
    args = None
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("nginx version:"):
            version = line.split(":", 1)[1].strip()
        elif line.startswith("configure arguments:"):
            args = split_shell_words(line.split(":", 1)[1])
    if not version:
        raise CheckError("nginx -V missing version line", text)
    if args is None:
        raise CheckError("nginx -V missing configure arguments", text)
    return version, args


def validate_nginx_report(version, args):
    if version != PINNED_VERSION:
        raise CheckError(f"nginx version is {version}, expected {PINNED_VERSION}")
    for required in REQUIRED_CONFIGURE_ARGS:
        if required not in args:
            raise CheckError(f"nginx configure arguments missing {required}")


def require_absolute_file(path, what, executable=False):
    if not path or not os.path.isabs(path):
        raise CheckError(f"{what} path must be absolute: {path!r}")
    if not os.path.isfile(path):
        raise CheckError(f"{what} is not a regular file: {path}")
    if executable and not os.access(path, os.X_OK):
        raise CheckError(f"{what} is not executable: {path}")
    return os.path.realpath(path)


def fixture(name):
    path = os.path.join(FIXTURES, name)
    if not os.path.isfile(path):
        raise CheckError(f"missing fixture {name}")
    return path


def combined_output(proc):
    out = proc.stdout or b""
    err = proc.stderr or b""
    return (out + err).decode("utf-8", "replace")


def nginx_test(nginx, prefix):
    return subprocess.run(
        [nginx, "-t", "-p", prefix, "-c", "conf/nginx.conf"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def write_text(path, body):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(body)


def write_index(prefix, rel, body):
    write_text(os.path.join(prefix, "html", rel), body)


def skeleton_conf(module, prefix, locations, port=18080, access_log=False, error="emerg"):
    module_q = quote_nginx_path(module)
    pid_q = quote_nginx_path(os.path.join(prefix, "logs", "nginx.pid"))
    html_q = quote_nginx_path(os.path.join(prefix, "html"))
    error_q = quote_nginx_path(os.path.join(prefix, "logs", "error.log"))
    access_q = quote_nginx_path(os.path.join(prefix, "logs", "access.log"))
    access_line = f"access_log {access_q};" if access_log else "access_log off;"
    return f"""
worker_processes 1;
error_log {error_q} {error};
pid {pid_q};
daemon off;
master_process on;
load_module {module_q};

events {{
    worker_connections 64;
}}

http {{
    {access_line}
    default_type text/plain;
    server {{
        listen 127.0.0.1:{port};
        server_name localhost;
        root {html_q};
{locations}
    }}
}}
"""


def location(path, script):
    script_q = quote_nginx_path(script)
    return f"""
        location {path} {{
            access_by_rss_file {script_q};
        }}
"""


def content_location(path, script):
    script_q = quote_nginx_path(script)
    return f"""
        location {path} {{
            content_by_rss_file {script_q};
        }}
"""


def runtime_conf(module, prefix, port):
    locations = "".join(
        [
            location("/sleep/", fixture("sleep_30ms.rss")),
            location("/two/", fixture("two_sleeps_access.rss")),
            location("/slow/", fixture("sleep_2s.rss")),
            location("/alpha/", fixture("sleep_80ms.rss")),
            location("/beta/", fixture("sleep_30ms.rss")),
            location("/long/", fixture("sleep_30s.rss")),
            location("/iso-a/", fixture("request_iso.rss")),
            location("/iso-b/", fixture("request_iso.rss")),
            location("/ctx-iso-a/", fixture("ctx_iso.rss")),
            location("/ctx-iso-b/", fixture("ctx_iso.rss")),
            location("/ctx-sync/", fixture("ctx_sync.rss")),
            location("/ctx-persist/", fixture("ctx_persist.rss")),
            location("/task7-exit/", fixture("task7_exit403.rss")),
            location("/task7-redirect/", fixture("task7_redirect.rss")),
            location("/task7-sleep-exit/", fixture("task7_exit_sleep.rss")),
            content_location("/content/", fixture("content_phase.rss")),
            """
        location /task7-mut/ {
            set $writable "";
            access_by_rss_file %s;
        }
        location /task7-iso-a/ {
            set $writable "";
            access_by_rss_file %s;
        }
        location /task7-iso-b/ {
            set $writable "";
            access_by_rss_file %s;
        }
"""
            % (
                quote_nginx_path(fixture("task7_mutation.rss")),
                quote_nginx_path(fixture("task7_iso.rss")),
                quote_nginx_path(fixture("task7_iso.rss")),
            ),
            """
        location /plain/ {
        }
""",
        ]
    )
    return skeleton_conf(
        module, prefix, locations, port=port, access_log=True, error="info"
    )


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        sock.listen(1)
        return sock.getsockname()[1]


def wait_port(port, timeout, proc=None):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc is not None and proc.poll() is not None:
            raise CheckError(f"nginx exited before listen (status {proc.returncode})")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise CheckError(f"nginx did not listen on 127.0.0.1:{port} within {timeout}s")


def read_file(path):
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as handle:
            return handle.read()
    except OSError:
        return ""


def parse_http(raw):
    head, sep, body = raw.partition("\r\n\r\n")
    if not sep:
        head, sep, body = raw.partition("\n\n")
    status = 0
    first = head.splitlines()[0] if head else ""
    parts = first.split()
    if len(parts) >= 2 and parts[1].isdigit():
        status = int(parts[1])
    content_length = None
    for line in head.splitlines()[1:]:
        name, colon, value = line.partition(":")
        if not colon:
            continue
        if name.strip().lower() == "content-length":
            digits = value.strip()
            if digits.isdigit():
                content_length = int(digits)
            break
    if content_length is not None:
        body = body[:content_length]
    return status, body


def http_get(port, path, timeout=10.0, sent_event=None):
    return http_exchange(port, "GET", path, timeout=timeout, sent_event=sent_event)


def http_exchange(
    port,
    method,
    path,
    timeout=10.0,
    sent_event=None,
    headers=None,
    version="HTTP/1.0",
):
    lines = [
        f"{method} {path} {version}",
        "Host: 127.0.0.1",
        "Connection: close",
    ]
    if headers:
        for name, value in headers.items():
            lines.append(f"{name}: {value}")
    req = ("\r\n".join(lines) + "\r\n\r\n").encode("ascii")
    sock = socket.create_connection(("127.0.0.1", port), timeout=timeout)
    try:
        sock.settimeout(timeout)
        sock.sendall(req)
        if sent_event is not None:
            sent_event.set()
        chunks = []
        while True:
            try:
                data = sock.recv(4096)
            except socket.timeout as exc:
                raise CheckError(f"timeout reading {path}") from exc
            if not data:
                break
            chunks.append(data)
    finally:
        sock.close()
    raw = b"".join(chunks).decode("latin1")
    status, body = parse_http(raw)
    return status, body, raw


def count_access(log_text, path, status):
    needle = f'"GET {path} HTTP/1.0" {status} '
    return log_text.count(needle)


def wait_access(log_path, path, status, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        text = read_file(log_path)
        if count_access(text, path, status):
            return text
        time.sleep(0.05)
    return read_file(log_path)


def dump_owned(prefix, proc, extra=""):
    parts = [extra] if extra else []
    parts.append("error.log:\n" + read_file(os.path.join(prefix, "logs", "error.log")))
    parts.append("access.log:\n" + read_file(os.path.join(prefix, "logs", "access.log")))
    master = os.path.join(prefix, "logs", "master.stderr")
    parts.append("master.stderr:\n" + read_file(master))
    if proc is not None and proc.poll() is not None:
        parts.append(f"nginx exit status: {proc.returncode}")
    return "\n".join(parts)


def expect_t_fail(nginx, prefix, module, locations, needles, label, any_needles=None):
    write_text(
        os.path.join(prefix, "conf", "nginx.conf"),
        skeleton_conf(module, prefix, locations),
    )
    out = nginx_test(nginx, prefix)
    text = combined_output(out)
    if out.returncode == 0:
        raise CheckError(f"{label}: expected nginx -t failure", text)
    lower = text.lower()
    for needle in needles:
        if needle.lower() not in lower:
            raise CheckError(
                f"{label}: expected cause {needle!r}",
                text,
            )
    if any_needles:
        if not any(needle.lower() in lower for needle in any_needles):
            raise CheckError(
                f"{label}: expected one of {any_needles!r}",
                text,
            )
    return text


def run_invalid_fixtures(nginx, prefix, module):
    missing = os.path.join(prefix, "conf", "no-such.rss")
    expect_t_fail(
        nginx,
        prefix,
        module,
        location("/", missing),
        ["script file not found"],
        "missing",
    )
    expect_t_fail(
        nginx,
        prefix,
        module,
        location("/", fixture("syntax_error.rss")),
        ["failed to compile script"],
        "syntax",
    )
    expect_t_fail(
        nginx,
        prefix,
        module,
        location("/", fixture("print.rss")),
        [],
        "import",
        any_needles=(
            "script imports are not supported",
            "failed to compile script",
        ),
    )
    expect_t_fail(
        nginx,
        prefix,
        module,
        """
        location /none {
            access_by_rss_file;
            return 200;
        }
""",
        ["invalid number of arguments"],
        "arity-none",
    )
    ok = quote_nginx_path(fixture("ok.rss"))
    expect_t_fail(
        nginx,
        prefix,
        module,
        f"""
        location /extra {{
            access_by_rss_file {ok} extra;
            return 200;
        }}
""",
        ["invalid number of arguments"],
        "arity-extra",
    )
    expect_t_fail(
        nginx,
        prefix,
        module,
        location("/", fixture("exec.rss")),
        [],
        "exec",
        any_needles=["ngx::exec", "unsupported import"],
    )
    expect_t_fail(
        nginx,
        prefix,
        module,
        content_location("/", missing),
        ["script file not found"],
        "content-missing",
    )
    expect_t_fail(
        nginx,
        prefix,
        module,
        content_location("/", fixture("print.rss")),
        [],
        "content-import",
        any_needles=(
            "script imports are not supported",
            "failed to compile script",
        ),
    )
    return ("missing", "syntax", "import", "arity", "exec", "content")


def spawn_nginx(nginx, prefix, handles):
    master_path = os.path.join(prefix, "logs", "master.stderr")
    handle = open(master_path, "wb")
    handles.append(handle)
    try:
        return subprocess.Popen(
            [nginx, "-p", prefix, "-c", "conf/nginx.conf"],
            stdin=subprocess.DEVNULL,
            stdout=handle,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
    except Exception:
        handle.close()
        raise


def close_handles(handles):
    while handles:
        handle = handles.pop()
        try:
            handle.close()
        except OSError:
            pass


def kill_owned_group(proc):
    if proc is None:
        return
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    except OSError as exc:
        if exc.errno == errno.ESRCH:
            pass
        else:
            try:
                proc.kill()
            except OSError:
                pass
            raise
    finally:
        try:
            proc.wait(timeout=2.0)
        except Exception:
            pass


def stop_owned(proc, handles, sig=signal.SIGTERM, timeout=2.0):
    try:
        if proc is None:
            return
        if proc.poll() is None and sig != signal.SIGKILL:
            try:
                proc.send_signal(sig)
                proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                pass
            except OSError as exc:
                if exc.errno != errno.ESRCH:
                    kill_owned_group(proc)
                    raise
        kill_owned_group(proc)
    finally:
        close_handles(handles)


def listen_closed(port, timeout=0.5):
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=timeout):
            return False
    except OSError:
        return True


def run_acceptance(nginx, module):
    tmp_root = task_tmp_root()
    os.makedirs(tmp_root, exist_ok=True)
    prefix = tempfile.mkdtemp(prefix="ngx-accept-", dir=tmp_root)
    proc = None
    handles = []
    summary = {}
    try:
        for child in ("logs", "conf", "html"):
            os.makedirs(os.path.join(prefix, child), exist_ok=True)

        write_index(prefix, "sleep/index.html", "slept\n")
        write_index(prefix, "two/index.html", "twice\n")
        write_index(prefix, "slow/index.html", "slow\n")
        write_index(prefix, "alpha/index.html", "alpha-body\n")
        write_index(prefix, "beta/index.html", "beta-body\n")
        write_index(prefix, "long/index.html", "long\n")
        write_index(prefix, "plain/index.html", "plain\n")
        write_index(prefix, "iso-a/index.html", "iso-a\n")
        write_index(prefix, "iso-b/index.html", "iso-b\n")
        write_index(prefix, "ctx-iso-a/index.html", "ctx-iso-a\n")
        write_index(prefix, "ctx-iso-b/index.html", "ctx-iso-b\n")
        write_index(prefix, "ctx-sync/index.html", "ctx-sync\n")
        write_index(prefix, "ctx-persist/index.html", "ctx-persist\n")
        write_index(prefix, "task7-iso-a/index.html", "task7-iso-a\n")
        write_index(prefix, "task7-iso-b/index.html", "task7-iso-b\n")
        write_index(prefix, "content/index.html", "content-static\n")

        write_text(
            os.path.join(prefix, "conf", "nginx.conf"),
            skeleton_conf(
                module,
                prefix,
                location("/", fixture("ngx_sleep.rss"))
                + content_location("/content-t/", fixture("content_phase.rss"))
                + """
        location /plain/ {
            return 200 "ok\\n";
        }
""",
            ),
        )
        valid = nginx_test(nginx, prefix)
        valid_text = combined_output(valid)
        if valid.returncode != 0:
            raise CheckError("valid nginx -t failed", valid_text)
        summary["nginx-t-valid"] = "ok"

        summary["nginx-t-invalid"] = ",".join(
            run_invalid_fixtures(nginx, prefix, module)
        )

        port = free_port()
        write_text(
            os.path.join(prefix, "conf", "nginx.conf"),
            runtime_conf(module, prefix, port),
        )
        runtime_t = nginx_test(nginx, prefix)
        runtime_text = combined_output(runtime_t)
        if runtime_t.returncode != 0:
            raise CheckError("runtime nginx -t failed", runtime_text)

        proc = spawn_nginx(nginx, prefix, handles)
        try:
            wait_port(port, 10.0, proc=proc)
        except CheckError as err:
            raise CheckError(
                str(err), dump_owned(prefix, proc, runtime_text)
            ) from err

        access_log = os.path.join(prefix, "logs", "access.log")

        content_status, _content_body, content_raw = http_get(port, "/content/", timeout=5.0)
        if content_status != 200:
            raise CheckError(
                "content_by_rss_file must return HTTP 200",
                dump_owned(prefix, proc, content_raw),
            )
        error_text = read_file(os.path.join(prefix, "logs", "error.log"))
        if "CONTENT_PHASE=content" not in error_text:
            raise CheckError(
                "content script must log CONTENT_PHASE=content",
                dump_owned(prefix, proc, error_text),
            )
        summary["content"] = "200 CONTENT_PHASE=content"

        # Non-blocking: 2s sleep must not hold /plain/.
        sent = threading.Event()
        slow_holder = {}

        def slow_worker():
            started = time.monotonic()
            try:
                status, body, raw = http_get(
                    port, "/slow/", timeout=12.0, sent_event=sent
                )
                slow_holder["status"] = status
                slow_holder["body"] = body
                slow_holder["raw"] = raw
                slow_holder["elapsed"] = time.monotonic() - started
            except Exception as exc:
                slow_holder["error"] = exc
                sent.set()
            finally:
                slow_holder["finished"] = time.monotonic()

        slow_thread = threading.Thread(target=slow_worker, name="slow")
        slow_thread.start()
        if not sent.wait(timeout=3.0):
            raise CheckError(
                "slow request bytes were not sent",
                dump_owned(prefix, proc),
            )
        plain_started = time.monotonic()
        plain_status, plain_body, plain_raw = http_get(port, "/plain/", timeout=5.0)
        plain_finished = time.monotonic()
        plain_elapsed = plain_finished - plain_started
        if not slow_thread.is_alive() or "finished" in slow_holder:
            raise CheckError(
                "slow request was no longer in flight after plain returned",
                dump_owned(prefix, proc),
            )
        if plain_status != 200 or plain_body != "plain\n":
            raise CheckError(
                "plain must return HTTP 200 before the sleeping response",
                dump_owned(prefix, proc, plain_raw),
            )
        if plain_elapsed >= 1.5:
            raise CheckError(
                f"plain completed in {plain_elapsed:.3f}s, not well below 2s sleep",
                dump_owned(prefix, proc),
            )
        slow_thread.join(timeout=12.0)
        if slow_thread.is_alive():
            raise CheckError("slow request did not finish", dump_owned(prefix, proc))
        if "error" in slow_holder:
            raise CheckError(
                f"slow request failed: {slow_holder['error']}",
                dump_owned(prefix, proc),
            )
        if slow_holder.get("status") != 200 or slow_holder.get("body") != "slow\n":
            raise CheckError(
                "sleeping route must return 200/slow after plain",
                dump_owned(prefix, proc, slow_holder.get("raw", "")),
            )
        slow_finished = slow_holder.get("finished")
        if slow_finished is None or slow_finished <= plain_finished:
            raise CheckError(
                "slow must finish after plain",
                dump_owned(prefix, proc),
            )
        summary["nonblock"] = (
            f"plain=200/plain before slow=200/slow elapsed_plain={plain_elapsed:.3f}s"
        )

        # Continuation: one-sleep and two-sleep distinct bodies.
        one_status, one_body, one_raw = http_get(port, "/sleep/", timeout=5.0)
        two_status, two_body, two_raw = http_get(port, "/two/", timeout=5.0)
        if one_status != 200 or one_body != "slept\n":
            raise CheckError(
                "one-sleep route must return 200/slept",
                dump_owned(prefix, proc, one_raw),
            )
        if two_status != 200 or two_body != "twice\n":
            raise CheckError(
                "two-sleep route must return 200/twice",
                dump_owned(prefix, proc, two_raw),
            )
        access_after_cont = read_file(access_log)
        one_logs = count_access(access_after_cont, "/sleep/", 200)
        two_logs = count_access(access_after_cont, "/two/", 200)
        if one_logs != 1 or two_logs != 1:
            raise CheckError(
                f"expected one access-log record each, got sleep={one_logs} two={two_logs}",
                dump_owned(prefix, proc, access_after_cont),
            )
        summary["continue"] = (
            f"sleep=200/slept two=200/twice access=1,1"
        )

        # Concurrent mapping across two routes/durations/bodies.
        jobs = [("/alpha/", "alpha-body\n")] * 6 + [("/beta/", "beta-body\n")] * 6
        barrier = threading.Barrier(len(jobs), timeout=5.0)
        results = [{} for _ in jobs]
        errors = []
        errors_lock = threading.Lock()

        def mapped(index, path, expected):
            try:
                barrier.wait()
                status, body, raw = http_get(port, path, timeout=8.0)
                results[index]["path"] = path
                results[index]["status"] = status
                results[index]["body"] = body
                results[index]["raw"] = raw
                other = "beta-body\n" if expected == "alpha-body\n" else "alpha-body\n"
                if status != 200 or body != expected:
                    with errors_lock:
                        errors.append((path, status, body))
                elif other in body:
                    with errors_lock:
                        errors.append((path, "mixed", body))
            except Exception as exc:
                with errors_lock:
                    errors.append((path, "exc", str(exc)))

        threads = [
            threading.Thread(target=mapped, args=(i, path, expected))
            for i, (path, expected) in enumerate(jobs)
        ]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=10.0)
        if any(thread.is_alive() for thread in threads):
            raise CheckError("concurrent requests did not finish", dump_owned(prefix, proc))
        if any(not item.get("status") for item in results):
            raise CheckError(
                "concurrent requests missing results",
                dump_owned(prefix, proc, str(errors)),
            )
        if errors:
            raise CheckError(
                f"concurrent mapping mismatch: {errors[:4]}",
                dump_owned(prefix, proc),
            )
        after_plain_status, after_plain_body, after_plain_raw = http_get(
            port, "/plain/", timeout=5.0
        )
        if after_plain_status != 200 or after_plain_body != "plain\n":
            raise CheckError(
                "plain must remain 200 after concurrent mapping",
                dump_owned(prefix, proc, after_plain_raw),
            )
        summary["concurrent"] = "12/12 mapped plain=200/plain"

        iso_barrier = threading.Barrier(2, timeout=5.0)
        iso_results = {}
        iso_errors = []
        iso_lock = threading.Lock()

        def iso_one(name, method, path, token):
            try:
                iso_barrier.wait()
                status, body, raw = http_exchange(
                    port,
                    method,
                    path,
                    timeout=8.0,
                    headers={"X-Token": token},
                )
                with iso_lock:
                    iso_results[name] = (status, body, raw)
                    if name == "iso-a":
                        if status != 200 or body != f"{name}\n":
                            iso_errors.append((name, status, body))
                    elif status not in (200, 405):
                        iso_errors.append((name, status, body))
            except Exception as exc:
                with iso_lock:
                    iso_errors.append((name, "exc", str(exc)))

        iso_threads = [
            threading.Thread(
                target=iso_one, args=("iso-a", "GET", "/iso-a/?token=alpha", "alpha")
            ),
            threading.Thread(
                target=iso_one, args=("iso-b", "POST", "/iso-b/?token=beta", "beta")
            ),
        ]
        for thread in iso_threads:
            thread.start()
        for thread in iso_threads:
            thread.join(timeout=10.0)
        if any(thread.is_alive() for thread in iso_threads):
            raise CheckError(
                "request-read isolation requests did not finish",
                dump_owned(prefix, proc),
            )
        if iso_errors:
            raise CheckError(
                f"request-read isolation HTTP mismatch: {iso_errors}",
                dump_owned(prefix, proc),
            )
        error_text = read_file(os.path.join(prefix, "logs", "error.log"))
        a_lines = [
            line
            for line in error_text.splitlines()
            if "/iso-a/?token=alpha" in line and ("ISO_METHOD=" in line or "ISO_VARS=" in line)
        ]
        b_lines = [
            line
            for line in error_text.splitlines()
            if "/iso-b/?token=beta" in line and ("ISO_METHOD=" in line or "ISO_VARS=" in line)
        ]
        if not any("ISO_METHOD=GET" in line for line in a_lines):
            raise CheckError(
                "iso-a must log GET without borrowing iso-b",
                dump_owned(prefix, proc, error_text),
            )
        if not any("ISO_METHOD=POST" in line for line in b_lines):
            raise CheckError(
                "iso-b must log POST without borrowing iso-a",
                dump_owned(prefix, proc, error_text),
            )
        if any("beta" in line for line in a_lines):
            raise CheckError(
                "iso-a logs must not contain iso-b token/header",
                dump_owned(prefix, proc, "\n".join(a_lines)),
            )
        if any("alpha" in line for line in b_lines):
            raise CheckError(
                "iso-b logs must not contain iso-a token/header",
                dump_owned(prefix, proc, "\n".join(b_lines)),
            )
        if not any("token=alpha" in line and "hdr=alpha" in line for line in a_lines):
            raise CheckError(
                "iso-a must log its own arg and header",
                dump_owned(prefix, proc, "\n".join(a_lines)),
            )
        if not any("token=beta" in line and "hdr=beta" in line for line in b_lines):
            raise CheckError(
                "iso-b must log its own arg and header",
                dump_owned(prefix, proc, "\n".join(b_lines)),
            )

        ctx_barrier = threading.Barrier(2, timeout=5.0)
        ctx_results = {}
        ctx_errors = []
        ctx_lock = threading.Lock()

        def ctx_one(name, path):
            try:
                ctx_barrier.wait()
                status, body, raw = http_get(port, path, timeout=8.0)
                with ctx_lock:
                    ctx_results[name] = (status, body, raw)
                    if status != 200:
                        ctx_errors.append((name, status, body))
            except Exception as exc:
                with ctx_lock:
                    ctx_errors.append((name, "exc", str(exc)))

        ctx_threads = [
            threading.Thread(target=ctx_one, args=("ctx-a", "/ctx-iso-a/?id=alpha")),
            threading.Thread(target=ctx_one, args=("ctx-b", "/ctx-iso-b/?id=beta")),
        ]
        for thread in ctx_threads:
            thread.start()
        for thread in ctx_threads:
            thread.join(timeout=10.0)
        if any(thread.is_alive() for thread in ctx_threads):
            raise CheckError(
                "ctx isolation requests did not finish",
                dump_owned(prefix, proc),
            )
        if ctx_errors:
            raise CheckError(
                f"ctx isolation HTTP mismatch: {ctx_errors}",
                dump_owned(prefix, proc),
            )
        error_text = read_file(os.path.join(prefix, "logs", "error.log"))
        ctx_a_lines = [
            line
            for line in error_text.splitlines()
            if "/ctx-iso-a/?id=alpha" in line and "CTX_ISO=" in line
        ]
        ctx_b_lines = [
            line
            for line in error_text.splitlines()
            if "/ctx-iso-b/?id=beta" in line and "CTX_ISO=" in line
        ]
        if not any("id=alpha" in line for line in ctx_a_lines):
            raise CheckError(
                "ctx-iso-a must log its own ctx id",
                dump_owned(prefix, proc, error_text),
            )
        if not any("id=beta" in line for line in ctx_b_lines):
            raise CheckError(
                "ctx-iso-b must log its own ctx id",
                dump_owned(prefix, proc, error_text),
            )
        if any("id=beta" in line for line in ctx_a_lines):
            raise CheckError(
                "ctx-iso-a logs must not contain ctx-iso-b id",
                dump_owned(prefix, proc, "\n".join(ctx_a_lines)),
            )
        if any("id=alpha" in line for line in ctx_b_lines):
            raise CheckError(
                "ctx-iso-b logs must not contain ctx-iso-a id",
                dump_owned(prefix, proc, "\n".join(ctx_b_lines)),
            )

        status, body, raw = http_get(port, "/ctx-sync/", timeout=8.0)
        if status != 200:
            raise CheckError(
                f"ctx-sync expected 200, got {status}",
                dump_owned(prefix, proc, raw),
            )
        error_text = read_file(os.path.join(prefix, "logs", "error.log"))
        if "CTX_SYNC=ok" not in error_text:
            raise CheckError(
                "ctx-sync must log CTX_SYNC=ok",
                dump_owned(prefix, proc, error_text),
            )

        status, body, raw = http_get(port, "/ctx-persist/", timeout=8.0)
        if status != 200:
            raise CheckError(
                f"ctx-persist expected 200, got {status}",
                dump_owned(prefix, proc, raw),
            )
        error_text = read_file(os.path.join(prefix, "logs", "error.log"))
        if not (
            "CTX_PERSIST=" in error_text
            and "n=2" in error_text
            and "k=v" in error_text
        ):
            raise CheckError(
                "ctx-persist must log persisted values after two waits",
                dump_owned(prefix, proc, error_text),
            )
        if not (
            "CTX_USER=" in error_text
            and "id=7" in error_text
            and "name=ada" in error_text
        ):
            raise CheckError(
                "ctx-persist must log nested user after two waits",
                dump_owned(prefix, proc, error_text),
            )
        summary["ctx"] = "sync=ok persist=ok"
        summary["concurrent"] = "12/12 mapped plain=200/plain isolation=ok"

        status, body, raw = http_get(port, "/task7-exit/", timeout=8.0)
        if status != 403:
            raise CheckError(
                f"task7-exit expected 403, got {status}",
                dump_owned(prefix, proc, raw),
            )
        error_text = read_file(os.path.join(prefix, "logs", "error.log"))
        if "TASK7_EXIT_MARKER" in error_text:
            raise CheckError(
                "task7-exit must not execute statements after ngx::exit",
                dump_owned(prefix, proc, error_text),
            )

        status, body, raw = http_get(port, "/task7-redirect/", timeout=8.0)
        if status != 302:
            raise CheckError(
                f"task7-redirect expected 302, got {status}",
                dump_owned(prefix, proc, raw),
            )
        if "http://example.test/x" not in raw:
            raise CheckError(
                "task7-redirect must set Location",
                dump_owned(prefix, proc, raw),
            )
        error_text = read_file(os.path.join(prefix, "logs", "error.log"))
        if "TASK7_REDIR_MARKER" in error_text:
            raise CheckError(
                "task7-redirect must not execute statements after ngx::redirect",
                dump_owned(prefix, proc, error_text),
            )

        status, body, raw = http_get(port, "/task7-sleep-exit/", timeout=8.0)
        if status != 401:
            raise CheckError(
                f"task7-sleep-exit expected 401, got {status}",
                dump_owned(prefix, proc, raw),
            )
        error_text = read_file(os.path.join(prefix, "logs", "error.log"))
        if "TASK7_SLEEP_EXIT_MARKER" in error_text:
            raise CheckError(
                "task7-sleep-exit must not execute statements after ngx::exit",
                dump_owned(prefix, proc, error_text),
            )

        status, body, raw = http_get(port, "/task7-mut/", timeout=8.0)
        if status != 201:
            raise CheckError(
                f"task7-mut expected 201, got {status}",
                dump_owned(prefix, proc, raw),
            )
        if "x-resp: ok" not in raw.lower():
            raise CheckError(
                "task7-mut must send response header X-Resp: ok",
                dump_owned(prefix, proc, raw),
            )
        error_text = read_file(os.path.join(prefix, "logs", "error.log"))
        state_lines = [
            line
            for line in error_text.splitlines()
            if "TASK7_MUT_STATE" in line
        ]
        if not state_lines:
            raise CheckError(
                "task7-mut must log TASK7_MUT_STATE from post-mutation reads",
                dump_owned(prefix, proc, error_text),
            )
        state = "\n".join(state_lines)
        missing = [
            name
            for name, needle in (
                ("method", "method=POST"),
                ("uri", "uri=%2Fmutated"),
                ("args", "args=k%3Dv"),
                ("host", "host=mut.example"),
                ("var", "var=from-rss"),
                ("status", "status=201"),
                ("resp", "resp=ok"),
            )
            if needle not in state
        ]
        if missing:
            raise CheckError(
                f"task7-mut missing TASK7_MUT_STATE fields {missing}",
                dump_owned(prefix, proc, state),
            )
        if "TASK7_MUT_MARKER" in error_text:
            raise CheckError(
                "task7-mut must not execute statements after ngx::exit",
                dump_owned(prefix, proc, error_text),
            )

        iso_barrier = threading.Barrier(2, timeout=5.0)
        iso_errors = []
        iso_lock = threading.Lock()

        def task7_iso_one(path, token):
            try:
                iso_barrier.wait()
                status, body, raw = http_get(port, path, timeout=8.0)
                if status != 200:
                    with iso_lock:
                        iso_errors.append((path, status, raw))
            except Exception as exc:
                with iso_lock:
                    iso_errors.append((path, "exc", str(exc)))

        iso_threads = [
            threading.Thread(target=task7_iso_one, args=("/task7-iso-a/?id=alpha", "alpha")),
            threading.Thread(target=task7_iso_one, args=("/task7-iso-b/?id=beta", "beta")),
        ]
        for thread in iso_threads:
            thread.start()
        for thread in iso_threads:
            thread.join(timeout=10.0)
        if iso_errors:
            raise CheckError(
                f"task7 isolation requests failed: {iso_errors[:2]}",
                dump_owned(prefix, proc),
            )
        error_text = read_file(os.path.join(prefix, "logs", "error.log"))
        iso_a_lines = [
            line
            for line in error_text.splitlines()
            if "/task7-iso-a/?id=alpha" in line and "TASK7_ISO=" in line
        ]
        iso_b_lines = [
            line
            for line in error_text.splitlines()
            if "/task7-iso-b/?id=beta" in line and "TASK7_ISO=" in line
        ]
        if not any("id=alpha" in line for line in iso_a_lines):
            raise CheckError(
                "task7-iso-a must log its own id",
                dump_owned(prefix, proc, error_text),
            )
        if not any("id=beta" in line for line in iso_b_lines):
            raise CheckError(
                "task7-iso-b must log its own id",
                dump_owned(prefix, proc, error_text),
            )
        if any("id=beta" in line for line in iso_a_lines):
            raise CheckError(
                "task7-iso-a logs must not contain beta id",
                dump_owned(prefix, proc, "\n".join(iso_a_lines)),
            )
        if any("id=alpha" in line for line in iso_b_lines):
            raise CheckError(
                "task7-iso-b logs must not contain alpha id",
                dump_owned(prefix, proc, "\n".join(iso_b_lines)),
            )
        summary["task7"] = (
            "exit=403 redirect=302 sleep-exit=401 mutation=201 isolation=ok exec-config=rejected"
        )

        # Disconnect during 30s sleep -> access log 499, then routes still work.
        disc = socket.create_connection(("127.0.0.1", port), timeout=3.0)
        try:
            disc.settimeout(3.0)
            disc.sendall(
                b"GET /long/ HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            )
            time.sleep(0.2)
        finally:
            disc.close()
        disc_started = time.monotonic()
        access_text = wait_access(access_log, "/long/", 499, 20.0)
        disc_elapsed = time.monotonic() - disc_started
        if not count_access(access_text, "/long/", 499):
            raise CheckError(
                "client close must log access status 499",
                dump_owned(prefix, proc, access_text),
            )
        if disc_elapsed >= 15.0:
            raise CheckError(
                "499 arrived too late versus the 30s sleep",
                dump_owned(prefix, proc),
            )
        disc_plain_status, disc_plain_body, disc_plain_raw = http_get(
            port, "/plain/", timeout=5.0
        )
        disc_sleep_status, disc_sleep_body, disc_sleep_raw = http_get(
            port, "/sleep/", timeout=5.0
        )
        if disc_plain_status != 200 or disc_plain_body != "plain\n":
            raise CheckError(
                "plain must succeed after disconnect",
                dump_owned(prefix, proc, disc_plain_raw),
            )
        if disc_sleep_status != 200 or disc_sleep_body != "slept\n":
            raise CheckError(
                "normal sleep must succeed after disconnect",
                dump_owned(prefix, proc, disc_sleep_raw),
            )
        summary["disconnect"] = "499 then plain=200/plain sleep=200/slept"

        # Graceful SIGQUIT with a long sleep in flight.
        quit_sent = threading.Event()
        hold_socks = []

        def hold_long():
            sock = socket.create_connection(("127.0.0.1", port), timeout=3.0)
            hold_socks.append(sock)
            sock.sendall(
                b"GET /long/ HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            )
            quit_sent.set()
            try:
                sock.settimeout(8.0)
                while True:
                    data = sock.recv(4096)
                    if not data:
                        break
            except OSError:
                pass

        hold_thread = threading.Thread(target=hold_long, name="hold-long")
        hold_thread.start()
        if not quit_sent.wait(timeout=3.0):
            raise CheckError(
                "long sleep request bytes were not sent before SIGQUIT",
                dump_owned(prefix, proc),
            )
        quit_started = time.monotonic()
        proc.send_signal(signal.SIGQUIT)
        try:
            quit_status = proc.wait(timeout=8.0)
        except subprocess.TimeoutExpired as exc:
            stop_owned(proc, handles, sig=signal.SIGKILL, timeout=2.0)
            proc = None
            raise CheckError(
                "SIGQUIT did not exit within 8s",
                dump_owned(prefix, None),
            ) from exc
        quit_elapsed = time.monotonic() - quit_started
        close_handles(handles)
        if quit_status != 0:
            raise CheckError(
                f"SIGQUIT exit status {quit_status}",
                dump_owned(prefix, proc),
            )
        if quit_elapsed >= 8.0:
            raise CheckError(
                f"SIGQUIT hung ({quit_elapsed:.3f}s)",
                dump_owned(prefix, proc),
            )
        if not listen_closed(port):
            raise CheckError(
                "listen port still accepted connections after SIGQUIT",
                dump_owned(prefix, proc),
            )
        summary["quit"] = f"status={quit_status}"
        for hold_sock in hold_socks:
            try:
                hold_sock.close()
            except OSError:
                pass
        hold_thread.join(timeout=8.0)
        proc = None
        return summary
    finally:
        stop_owned(proc, handles)
        shutil.rmtree(prefix, ignore_errors=True)


def format_summary(summary):
    lines = [
        "nginx-integration ok",
        f"nginx-t: valid={summary['nginx-t-valid']} invalid={summary['nginx-t-invalid']}",
        f"content: {summary['content']}",
        f"nonblock: {summary['nonblock']}",
        f"continue: {summary['continue']}",
        f"concurrent: {summary['concurrent']}",
        f"task7: {summary['task7']}",
        f"disconnect: {summary['disconnect']}",
        f"quit: {summary['quit']}",
    ]
    return "\n".join(lines)


def main(argv):
    parser = argparse.ArgumentParser(
        description="Asynchronous NGINX acceptance for ngx-rustscript"
    )
    parser.add_argument("--nginx", required=True, help="absolute pinned nginx binary")
    parser.add_argument("--module", required=True, help="absolute module cdylib")
    args = parser.parse_args(argv)

    nginx = require_absolute_file(args.nginx, "nginx", executable=True)
    module = require_absolute_file(args.module, "module")

    version_proc = subprocess.run(
        [nginx, "-V"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    version_text = combined_output(version_proc)
    if version_proc.returncode != 0:
        raise CheckError("nginx -V failed", version_text)
    version, configure_args = parse_nginx_v(version_text)
    validate_nginx_report(version, configure_args)

    summary = run_acceptance(nginx, module)
    print(format_summary(summary))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except CheckError as err:
        sys.stderr.write(f"nginx-integration failed: {err}\n")
        if err.detail:
            sys.stderr.write(err.detail.rstrip() + "\n")
        sys.exit(1)
