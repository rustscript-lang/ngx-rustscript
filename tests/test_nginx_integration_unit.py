#!/usr/bin/env python3
"""Small stdlib checks for nginx_integration helpers. No live NGINX."""

import os
import signal
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import nginx_integration as ni  # noqa: E402


class QuoteTests(unittest.TestCase):
    def test_quotes_spaces(self):
        self.assertEqual(
            ni.quote_nginx_path("/tmp/foo bar/mod.so"),
            '"/tmp/foo bar/mod.so"',
        )

    def test_escapes_quotes_and_backslashes(self):
        self.assertEqual(
            ni.quote_nginx_path('/tmp/foo"bar\\baz'),
            '"/tmp/foo\\"bar\\\\baz"',
        )


class NginxVTests(unittest.TestCase):
    def test_accepts_pinned_openresty_1_31_4_1(self):
        text = (
            "nginx version: openresty/1.31.4.1\n"
            "built by gcc 13.3.0\n"
            "configure arguments: --prefix=/tmp/nginx-prefix "
            "--with-http_ssl_module\n"
        )
        version, args = ni.parse_nginx_v(text)
        ni.validate_nginx_report(version, args)

    def test_rejects_wrong_version(self):
        text = (
            "nginx version: nginx/1.24.0\n"
            "configure arguments: --prefix=/tmp/nginx-prefix "
            "--with-compat --with-http_ssl_module\n"
        )
        version, args = ni.parse_nginx_v(text)
        with self.assertRaises(ni.CheckError):
            ni.validate_nginx_report(version, args)

    def test_rejects_missing_ssl(self):
        text = (
            "nginx version: openresty/1.31.4.1\n"
            "configure arguments: --prefix=/tmp/nginx-prefix --with-debug\n"
        )
        version, args = ni.parse_nginx_v(text)
        with self.assertRaises(ni.CheckError):
            ni.validate_nginx_report(version, args)

    def test_quoted_prefix_with_spaces(self):
        text = (
            "nginx version: openresty/1.31.4.1\n"
            "configure arguments: --prefix='/tmp/pre fix' "
            "--with-debug --with-http_ssl_module\n"
        )
        version, args = ni.parse_nginx_v(text)
        self.assertEqual(
            args,
            [
                "--prefix=/tmp/pre fix",
                "--with-debug",
                "--with-http_ssl_module",
            ],
        )
        ni.validate_nginx_report(version, args)


class PathTests(unittest.TestCase):
    def test_rejects_relative_nginx(self):
        with self.assertRaises(ni.CheckError):
            ni.require_absolute_file("nginx", "nginx", executable=True)

    def test_rejects_missing_module(self):
        with self.assertRaises(ni.CheckError):
            ni.require_absolute_file("/no/such/libngx_rustscript.so", "module")


class ParseHttpTests(unittest.TestCase):
    def test_content_length_truncates_body(self):
        raw = "HTTP/1.0 200 OK\r\nContent-Length: 6\r\n\r\nplain\nTRAILING"
        status, body = ni.parse_http(raw)
        self.assertEqual(status, 200)
        self.assertEqual(body, "plain\n")

    def test_without_content_length_keeps_close_body(self):
        raw = "HTTP/1.0 200 OK\r\n\r\nslept\n"
        status, body = ni.parse_http(raw)
        self.assertEqual(status, 200)
        self.assertEqual(body, "slept\n")


class TmpRootTests(unittest.TestCase):
    def test_defaults_to_tempdir(self):
        previous = os.environ.pop("NGX_RUSTSCRIPT_TASK_TMP", None)
        try:
            self.assertEqual(ni.task_tmp_root(), tempfile.gettempdir())
        finally:
            if previous is not None:
                os.environ["NGX_RUSTSCRIPT_TASK_TMP"] = previous

    def test_honors_env(self):
        previous = os.environ.get("NGX_RUSTSCRIPT_TASK_TMP")
        os.environ["NGX_RUSTSCRIPT_TASK_TMP"] = "/tmp/ngx-task-tmp-test"
        try:
            self.assertEqual(ni.task_tmp_root(), "/tmp/ngx-task-tmp-test")
        finally:
            if previous is None:
                os.environ.pop("NGX_RUSTSCRIPT_TASK_TMP", None)
            else:
                os.environ["NGX_RUSTSCRIPT_TASK_TMP"] = previous


class StopOwnedTests(unittest.TestCase):
    def test_kills_owned_group_when_master_already_reaped(self):
        recorded = []

        class ReapedProc:
            pid = 4242

            def poll(self):
                return 0

            def wait(self, timeout=None):
                return 0

            def send_signal(self, sig):
                raise AssertionError("reaped master must not be signaled")

            def kill(self):
                raise AssertionError("reaped master must not be signaled")

        def fake_killpg(pgid, sig):
            recorded.append((pgid, sig))

        original = ni.os.killpg
        ni.os.killpg = fake_killpg
        try:
            ni.stop_owned(ReapedProc(), [])
        finally:
            ni.os.killpg = original
        self.assertEqual(recorded, [(4242, signal.SIGKILL)])


if __name__ == "__main__":
    unittest.main()
