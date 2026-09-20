#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import os
import posixpath
import re
from pathlib import Path
import subprocess
import tarfile
import tempfile
import tomllib
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[1]
PACKAGE_SCRIPT = ROOT / "scripts" / "package_release.sh"
CI_WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
RELEASE_WORKFLOW = ROOT / ".github" / "workflows" / "release.yml"


def run(
    *args: str,
    env: dict[str, str] | None = None,
    cwd: Path = ROOT,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        cwd=cwd,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    digest.update(path.read_bytes())
    return digest.hexdigest()


def relative_markdown_link_errors(files: dict[str, bytes]) -> list[str]:
    link_re = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")
    errors: list[str] = []
    for name, data in files.items():
        if not name.endswith(".md"):
            continue
        for raw in link_re.findall(data.decode("utf-8")):
            target = raw.strip()
            if target.startswith("<") and target.endswith(">"):
                target = target[1:-1]
            if (
                target.startswith("#")
                or target.startswith("//")
                or re.match(r"^[a-z][a-z0-9+.-]*:", target, re.IGNORECASE)
            ):
                continue
            relative = target.split("#", 1)[0]
            if not relative:
                continue
            destination = posixpath.normpath(posixpath.join(posixpath.dirname(name), relative))
            if destination not in files and f"{destination}/index.md" not in files:
                errors.append(f"{name}: missing {target}")
    return errors


def tar_payload(path: Path, top: str) -> dict[str, bytes]:
    prefix = f"{top}/"
    payload: dict[str, bytes] = {}
    with tarfile.open(path, "r:gz") as archive:
        for member in archive.getmembers():
            if not member.isfile() or not member.name.startswith(prefix):
                continue
            extracted = archive.extractfile(member)
            if extracted is not None:
                payload[member.name[len(prefix) :]] = extracted.read()
    return payload


def directory_payload(path: Path) -> dict[str, bytes]:
    return {
        item.relative_to(path).as_posix(): item.read_bytes()
        for item in path.rglob("*")
        if item.is_file()
    }


FROZEN_RUSTSCRIPT_REV = "87d8c5d5cb42b3381aecadb383c0b7bf34ff204f"
STALE_RUSTSCRIPT_PINS = (
    "f5f71eb",
    "f5f71ebc2f38e276c35f2aebaf63f4e2780efe99",
)
NGINX_VERSION = "1.31.4"
OFFICIAL_NGINX_1_31_4_SHA256 = "e6f20b644a17a643f059ae6467a1971fe2811587d025e071068753a1f1e3b3c3"
FULL_GIT_SHA = re.compile(r"^[0-9a-f]{40}$")


def require_full_git_rev(rev: str, label: str) -> str:
    if not FULL_GIT_SHA.fullmatch(rev):
        raise AssertionError(f"{label} must be a full 40-character git SHA, got {rev!r}")
    if len(rev) != 40:
        raise AssertionError(f"{label} rejected a non-full pin: {rev!r}")
    return rev


class ReleasePackagingTests(unittest.TestCase):
    def test_rustscript_core_pin_is_frozen_full_sha(self) -> None:
        cargo_toml = (ROOT / "Cargo.toml").read_text()
        metadata = tomllib.loads(cargo_toml)["package"]["metadata"]["ngx-rustscript"]
        deps = tomllib.loads(cargo_toml)["dependencies"]
        lock = (ROOT / "Cargo.lock").read_text()

        metadata_rev = require_full_git_rev(metadata["rustscript-rev"], "package.metadata.ngx-rustscript.rustscript-rev")
        host_rev = require_full_git_rev(deps["pd-host-function"]["rev"], "pd-host-function.rev")
        vm_rev = require_full_git_rev(deps["vm"]["rev"], "pd-vm.rev")
        self.assertEqual(metadata_rev, FROZEN_RUSTSCRIPT_REV)
        self.assertEqual(host_rev, FROZEN_RUSTSCRIPT_REV)
        self.assertEqual(vm_rev, FROZEN_RUSTSCRIPT_REV)
        self.assertEqual(deps["vm"]["package"], "pd-vm")

        expected_source = (
            f"git+https://github.com/rustscript-lang/rustscript?rev={FROZEN_RUSTSCRIPT_REV}"
            f"#{FROZEN_RUSTSCRIPT_REV}"
        )
        self.assertIn(f'name = "pd-vm"', lock)
        self.assertIn(f'name = "pd-host-function"', lock)
        self.assertGreaterEqual(lock.count(expected_source), 2)
        with self.assertRaises(AssertionError):
            require_full_git_rev("b1d6cff", "abbreviated")
        for stale in STALE_RUSTSCRIPT_PINS:
            self.assertNotIn(stale, cargo_toml)
            self.assertNotIn(stale, lock)

        for public_pin in (ROOT / "README.md", ROOT / "docs/design/ngx-scheduler.md"):
            source = public_pin.read_text()
            self.assertIn(FROZEN_RUSTSCRIPT_REV, source, public_pin)
            for stale in STALE_RUSTSCRIPT_PINS:
                self.assertNotIn(stale, source, public_pin)

    def test_workflows_parse_and_cover_ci_cd_contract(self) -> None:
        self.assertTrue(CI_WORKFLOW.is_file())
        self.assertTrue(RELEASE_WORKFLOW.is_file())

        ci = yaml.load(CI_WORKFLOW.read_text(), Loader=yaml.BaseLoader)
        release = yaml.load(RELEASE_WORKFLOW.read_text(), Loader=yaml.BaseLoader)

        self.assertEqual(ci["permissions"]["contents"], "read")
        self.assertIn("pull_request", ci["on"])
        self.assertIn("workflow_call", ci["on"])
        self.assertIsInstance(ci["on"]["workflow_call"], dict)
        call_inputs = ci["on"]["workflow_call"]["inputs"]
        self.assertEqual(call_inputs["ref"]["type"], "string")
        self.assertEqual(ci["on"]["push"]["branches"], ["master"])
        self.assertIn("inputs.ref || github.ref", ci["concurrency"]["group"])
        self.assertIn("inputs.ref == ''", ci["concurrency"]["cancel-in-progress"])

        metadata = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["metadata"]["ngx-rustscript"]
        self.assertEqual(metadata["nginx-core-version"], NGINX_VERSION)
        self.assertEqual(
            metadata["nginx-core-tarball-sha256"],
            OFFICIAL_NGINX_1_31_4_SHA256,
        )
        self.assertEqual(ci["env"]["NGINX_CORE_VERSION"], NGINX_VERSION)
        self.assertEqual(
            ci["env"]["NGINX_CORE_TARBALL_SHA256"],
            OFFICIAL_NGINX_1_31_4_SHA256,
        )
        self.assertEqual(release["env"]["NGINX_CORE_VERSION"], NGINX_VERSION)
        self.assertEqual(
            release["env"]["NGINX_CORE_TARBALL_SHA256"],
            OFFICIAL_NGINX_1_31_4_SHA256,
        )

        ci_text = CI_WORKFLOW.read_text()
        release_text = RELEASE_WORKFLOW.read_text()
        for workflow_text in (ci_text, release_text):
            self.assertNotIn("openresty.org/download", workflow_text)
            self.assertNotIn("OPENRESTY_VERSION", workflow_text)
            self.assertIn(OFFICIAL_NGINX_1_31_4_SHA256, workflow_text)
        for required in (
            "cargo fmt --all -- --check",
            "cargo check --locked",
            "cargo clippy --locked --all-targets -- -D warnings",
            "--test-threads=1",
            "tests/nginx_integration.py",
            "tests/test_release_packaging.py",
            "https://nginx.org/download/nginx-${NGINX_CORE_VERSION}.tar.gz",
            "sha256sum --check --strict",
            "CFLAGS=-I%s",
            "BINDGEN_EXTRA_CLANG_ARGS=-I%s",
        ):
            self.assertIn(required, ci_text)

        self.assertEqual(release["permissions"]["contents"], "write")
        self.assertEqual(release["on"]["push"]["tags"], ["v*"])
        dispatch = release["on"]["workflow_dispatch"]
        self.assertEqual(dispatch["inputs"]["tag"]["required"], "true")
        self.assertEqual(release["jobs"]["verify"]["uses"], "./.github/workflows/ci.yml")
        self.assertEqual(release["jobs"]["verify"]["permissions"]["contents"], "read")
        self.assertIn("inputs.tag", release["jobs"]["verify"]["with"]["ref"])
        self.assertEqual(release["jobs"]["publish"]["needs"], "verify")
        publish_steps = release["jobs"]["publish"]["steps"]
        checkout = next(step for step in publish_steps if step.get("name") == "Check out release tag")
        self.assertEqual(checkout["with"]["persist-credentials"], "false")
        upload = next(step for step in publish_steps if step.get("name") == "Upload workflow artifact")
        self.assertEqual(upload["with"]["overwrite"], "true")
        for required in (
            "scripts/package_release.sh",
            "--nginx \"$NGX_RUSTSCRIPT_NGINX\"",
            "--nginx-source \"$NGINX_SOURCE_DIR\"",
            "gh release create",
            "gh release upload",
            "--clobber",
            "SHA256SUMS",
            "ngx-rustscript-module-${VERSION}-linux-amd64.tgz",
            "ngx-rustscript-nginx-${VERSION}-nginx-${NGINX_CORE_VERSION}-linux-amd64.tgz",
            "nginx-module-rustscript_${VERSION}_amd64.deb",
        ):
            self.assertIn(required, release_text)

    def test_packaging_outputs_expected_reproducible_assets(self) -> None:
        self.assertTrue(PACKAGE_SCRIPT.is_file())
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            module = tmp_path / "libngx_rustscript.so"
            module.write_bytes(b"\x7fELFngx-rustscript-test\n")
            nginx = tmp_path / "nginx"
            nginx.write_text("#!/bin/sh\nexit 0\n")
            nginx.chmod(0o755)
            nginx_source = tmp_path / "nginx-source"
            (nginx_source / "conf").mkdir(parents=True)
            (nginx_source / "html").mkdir()
            (nginx_source / "conf" / "mime.types").write_text("types {}\n")
            (nginx_source / "html" / "index.html").write_text("index\n")
            (nginx_source / "html" / "50x.html").write_text("error\n")
            (nginx_source / "LICENSE").write_text("nginx license\n")
            first = tmp_path / "first"
            second = tmp_path / "second"
            env = os.environ.copy()
            env["SOURCE_DATE_EPOCH"] = "1700000000"

            for output in (first, second):
                result = run(
                    str(PACKAGE_SCRIPT),
                    "--version",
                    "0.1.0",
                    "--arch",
                    "amd64",
                    "--module",
                    str(module),
                    "--nginx",
                    str(nginx),
                    "--nginx-source",
                    str(nginx_source),
                    "--output-dir",
                    str(output),
                    env=env,
                )
                self.assertEqual(result.returncode, 0, result.stdout)

            module_tgz_name = "ngx-rustscript-module-0.1.0-linux-amd64.tgz"
            bundle_tgz_name = "ngx-rustscript-nginx-0.1.0-nginx-1.31.4-linux-amd64.tgz"
            deb_name = "nginx-module-rustscript_0.1.0_amd64.deb"
            expected = {module_tgz_name, bundle_tgz_name, deb_name, "SHA256SUMS"}
            self.assertEqual({path.name for path in first.iterdir()}, expected)

            module_top = "ngx-rustscript-module-0.1.0-linux-amd64"
            with tarfile.open(first / module_tgz_name, "r:gz") as archive:
                member_list = archive.getmembers()
                members = {member.name: member for member in member_list}
            self.assertTrue(all(not member.issym() and not member.islnk() for member in member_list))
            self.assertTrue(all(".." not in Path(member.name).parts for member in member_list))
            for name in (
                f"{module_top}/lib/ngx_http_rustscript_module.so",
                f"{module_top}/README.md",
                f"{module_top}/docs/ngx-api-compatibility.md",
                f"{module_top}/docs/index.md",
                f"{module_top}/docs/directives.md",
                f"{module_top}/docs/api/index.md",
                f"{module_top}/docs/security.md",
                f"{module_top}/examples/rate-limit.rss",
                f"{module_top}/examples/key-auth.rss",
            ):
                self.assertIn(name, members)
            self.assertEqual(
                members[f"{module_top}/lib/ngx_http_rustscript_module.so"].mode,
                0o644,
            )
            self.assertEqual(
                relative_markdown_link_errors(tar_payload(first / module_tgz_name, module_top)),
                [],
            )

            bundle_top = "ngx-rustscript-nginx-0.1.0-nginx-1.31.4-linux-amd64"
            with tarfile.open(first / bundle_tgz_name, "r:gz") as archive:
                bundle_member_list = archive.getmembers()
                bundle_members = {member.name: member for member in bundle_member_list}
                bundle_conf_file = archive.extractfile(f"{bundle_top}/conf/nginx.conf")
                self.assertIsNotNone(bundle_conf_file)
                bundle_conf = bundle_conf_file.read() if bundle_conf_file is not None else b""
            self.assertTrue(
                all(not member.issym() and not member.islnk() for member in bundle_member_list)
            )
            self.assertTrue(all(".." not in Path(member.name).parts for member in bundle_member_list))
            for name in (
                f"{bundle_top}/bin/nginx-rustscript",
                f"{bundle_top}/sbin/nginx",
                f"{bundle_top}/modules/ngx_http_rustscript_module.so",
                f"{bundle_top}/conf/nginx.conf",
                f"{bundle_top}/conf/mime.types",
                f"{bundle_top}/html/index.html",
                f"{bundle_top}/licenses/nginx/LICENSE",
                f"{bundle_top}/docs/index.md",
                f"{bundle_top}/docs/directives.md",
                f"{bundle_top}/docs/api/index.md",
                f"{bundle_top}/examples/rate-limit.rss",
            ):
                self.assertIn(name, bundle_members)
            self.assertEqual(bundle_members[f"{bundle_top}/bin/nginx-rustscript"].mode, 0o755)
            self.assertEqual(bundle_members[f"{bundle_top}/sbin/nginx"].mode, 0o755)
            self.assertEqual(bundle_members[f"{bundle_top}/modules/ngx_http_rustscript_module.so"].mode, 0o644)
            self.assertIn(
                b"load_module modules/ngx_http_rustscript_module.so;",
                bundle_conf,
            )
            self.assertIn(b"include mime.types;", bundle_conf)
            self.assertNotIn(b"include conf/mime.types;", bundle_conf)
            self.assertEqual(
                relative_markdown_link_errors(tar_payload(first / bundle_tgz_name, bundle_top)),
                [],
            )

            deb = first / deb_name
            self.assertEqual(run("dpkg-deb", "-f", str(deb), "Package").stdout.strip(), "nginx-module-rustscript")
            self.assertEqual(run("dpkg-deb", "-f", str(deb), "Version").stdout.strip(), "0.1.0")
            self.assertEqual(run("dpkg-deb", "-f", str(deb), "Architecture").stdout.strip(), "amd64")
            self.assertEqual(run("dpkg-deb", "-f", str(deb), "Depends").stdout.strip(), "libc6, libgcc-s1")
            self.assertEqual(
                run("dpkg-deb", "-f", str(deb), "X-Ngx-Rustscript-Nginx-Version").stdout.strip(),
                "1.31.4",
            )
            contents = run("dpkg-deb", "-c", str(deb)).stdout
            self.assertIn("./usr/lib/nginx/modules/ngx_http_rustscript_module.so", contents)
            self.assertIn("./usr/share/doc/nginx-module-rustscript/examples/rate-limit.rss", contents)
            self.assertIn("./usr/share/doc/nginx-module-rustscript/docs/index.md", contents)
            self.assertIn("./usr/share/doc/nginx-module-rustscript/docs/directives.md", contents)
            self.assertIn("./usr/share/doc/nginx-module-rustscript/docs/api/index.md", contents)
            self.assertNotIn("./usr/share/doc/nginx-module-rustscript/ngx-api-compatibility.md", contents)
            self.assertNotIn("../", contents)
            self.assertFalse(any(line.startswith("l") for line in contents.splitlines()))
            deb_extract = tmp_path / "deb-extract"
            extract_result = run("dpkg-deb", "-x", str(deb), str(deb_extract))
            self.assertEqual(extract_result.returncode, 0, extract_result.stdout)
            deb_docs = deb_extract / "usr/share/doc/nginx-module-rustscript"
            self.assertEqual(relative_markdown_link_errors(directory_payload(deb_docs)), [])

            checksum_check = run("sha256sum", "--check", "--strict", "SHA256SUMS", cwd=first)
            self.assertEqual(checksum_check.returncode, 0, checksum_check.stdout)
            sums = {}
            for line in (first / "SHA256SUMS").read_text().splitlines():
                digest, filename = line.split("  ", 1)
                sums[filename] = digest
            self.assertEqual(set(sums), {module_tgz_name, bundle_tgz_name, deb_name})
            self.assertEqual(sums[module_tgz_name], sha256(first / module_tgz_name))
            self.assertEqual(sums[bundle_tgz_name], sha256(first / bundle_tgz_name))
            self.assertEqual(sums[deb_name], sha256(first / deb_name))

            for name in expected:
                self.assertEqual(sha256(first / name), sha256(second / name), name)

    def test_packaging_rejects_unsafe_version(self) -> None:
        self.assertTrue(PACKAGE_SCRIPT.is_file())
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            module = tmp_path / "module.so"
            module.write_bytes(b"module")
            result = run(
                str(PACKAGE_SCRIPT),
                "--version",
                "../bad",
                "--arch",
                "amd64",
                "--module",
                str(module),
                "--nginx",
                str(module),
                "--nginx-source",
                str(tmp_path),
                "--output-dir",
                str(tmp_path / "out"),
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("invalid version", result.stdout)


if __name__ == "__main__":
    unittest.main()
