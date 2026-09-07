//! Isolated NGINX fixture: missing module fails `nginx -t`, built module loads.

mod support;

use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

use support::artifact::module_cdylib;
use support::nginx::{
    combined, is_missing_load_module_error, nginx_binary, nginx_test, write_conf,
};
use support::temp::{TEMP_DIR_MODE, isolated_prefix};

#[test]
fn missing_dynamic_module_fails_nginx_t() {
    let nginx = nginx_binary();
    let prefix = isolated_prefix("missing");
    let missing = prefix.join("modules/libngx_rustscript-missing.so");
    assert!(!missing.exists(), "missing-module path must not exist");
    write_conf(&prefix, &missing);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        !out.status.success(),
        "nginx -t should fail when the module is missing\n{text}"
    );
    let conf = prefix.join("conf/nginx.conf");
    assert!(
        is_missing_load_module_error(&text, &missing, &conf),
        "expected load_module of {} to fail with that exact path, got:\n{text}",
        missing.display()
    );
}

#[test]
fn built_dynamic_module_passes_nginx_t() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let prefix = isolated_prefix("present");
    write_conf(&prefix, &module);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "nginx -t should succeed after loading the module\n{text}"
    );
    assert!(
        text.contains("test is successful"),
        "expected successful nginx -t output, got:\n{text}"
    );
}
#[test]
fn nginx_test_passes_non_utf8_prefix_as_os_str_without_panic() {
    let nginx = nginx_binary();
    let parent = isolated_prefix("non-utf8-parent");
    let mut dir = OsString::from(parent.as_os_str());
    dir.push(OsStr::from_bytes(b"/\xffprefix"));
    let prefix = PathBuf::from(dir);
    fs::create_dir(&prefix).unwrap();
    for child in ["logs", "conf", "html"] {
        fs::create_dir(prefix.join(child)).unwrap();
    }
    let _out = nginx_test(&nginx, &prefix);
}

#[test]
fn prefix_with_spaces_and_semicolon_passes_nginx_t() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let parent = isolated_prefix("space-parent");
    let prefix = parent.join("pre fix;ok");
    DirBuilder::new()
        .recursive(true)
        .mode(TEMP_DIR_MODE)
        .create(&prefix)
        .unwrap();
    for child in ["logs", "conf", "html"] {
        DirBuilder::new()
            .mode(TEMP_DIR_MODE)
            .create(prefix.join(child))
            .unwrap();
    }
    write_conf(&prefix, &module);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "nginx -t should accept quoted prefix with spaces/semicolon\n{text}"
    );
    assert!(text.contains("test is successful"), "got:\n{text}");
}

#[test]
fn prefix_with_quote_passes_nginx_t() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let parent = isolated_prefix("quote-parent");
    let prefix = parent.join("pre\"fix");
    DirBuilder::new()
        .recursive(true)
        .mode(TEMP_DIR_MODE)
        .create(&prefix)
        .unwrap();
    for child in ["logs", "conf", "html"] {
        DirBuilder::new()
            .mode(TEMP_DIR_MODE)
            .create(prefix.join(child))
            .unwrap();
    }
    write_conf(&prefix, &module);
    let out = nginx_test(&nginx, &prefix);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "nginx -t should accept escaped quote in prefix\n{text}"
    );
    assert!(text.contains("test is successful"), "got:\n{text}");
}
