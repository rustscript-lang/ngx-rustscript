//! `rss_shared_dict` config-time tests: compile, import, and `nginx -t`.

use ngx_rustscript_nginx_test_stubs as _;

mod support;

use std::fs;
use std::path::Path;

use ngx_rustscript::config::{compile_rss_source, program_requires_request_state};
use support::artifact::module_cdylib;
use support::nginx::{combined, nginx_binary, nginx_quote_path, nginx_test};
use support::temp::isolated_prefix;

fn write_conf(prefix: &Path, body: &str) {
    fs::write(prefix.join("conf/nginx.conf"), body).unwrap();
}

fn conf_with_http(module: &Path, prefix: &Path, http_extra: &str, locations: &str) -> String {
    let module = nginx_quote_path(module).unwrap();
    let pid = nginx_quote_path(&prefix.join("logs/nginx.pid")).unwrap();
    format!(
        r#"
worker_processes 1;
error_log stderr emerg;
pid {pid};
daemon off;
master_process off;
load_module {module};

events {{
    worker_connections 8;
}}

http {{
    access_log off;
    {http_extra}
    server {{
        listen 127.0.0.1:18765;
        server_name localhost;
{locations}
    }}
}}
"#
    )
}

fn write_script(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

#[test]
fn shared_dict_hosts_compile_without_request_state() {
    let program = compile_rss_source(
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "k", 1, null, null);
ngx::shared::get(d, "k");
"#,
    )
    .expect("shared dict script compiles");
    assert!(
        !program_requires_request_state(&program),
        "shared dict does not require request-owned VM state"
    );
}

#[test]
fn rss_shared_dict_nginx_t_accepts_valid_zone() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix("shared-dict-valid");
    write_conf(
        tmp.path(),
        &conf_with_http(
            &module,
            tmp.path(),
            "rss_shared_dict dogs 1m;",
            r#"
        location / {
            return 200;
        }
"#,
        ),
    );
    let out = nginx_test(&nginx, tmp.path());
    assert!(
        out.status.success(),
        "valid rss_shared_dict should pass nginx -t: {}",
        combined(&out)
    );
}

#[test]
fn rss_shared_dict_nginx_t_rejects_invalid_size() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix("shared-dict-size");
    write_conf(
        tmp.path(),
        &conf_with_http(
            &module,
            tmp.path(),
            "rss_shared_dict dogs 1k;",
            r#"
        location / {
            return 200;
        }
"#,
        ),
    );
    let out = nginx_test(&nginx, tmp.path());
    assert!(!out.status.success(), "1k is below the 8k minimum");
    let text = combined(&out);
    assert!(
        text.to_ascii_lowercase().contains("invalid size")
            || text.to_ascii_lowercase().contains("rss_shared_dict"),
        "unexpected nginx -t output: {text}"
    );
}

#[test]
fn rss_shared_dict_nginx_t_rejects_location_context() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix("shared-dict-loc");
    write_conf(
        tmp.path(),
        &conf_with_http(
            &module,
            tmp.path(),
            "",
            r#"
        location / {
            rss_shared_dict dogs 1m;
            return 200;
        }
"#,
        ),
    );
    let out = nginx_test(&nginx, tmp.path());
    assert!(
        !out.status.success(),
        "rss_shared_dict is http main only: {}",
        combined(&out)
    );
}

#[test]
fn rss_shared_dict_and_access_script_nginx_t() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix("shared-dict-script");
    let script = tmp.path().join("html/shared.rss");
    write_script(
        &script,
        r#"
use ngx;
let d = ngx::shared::dict("dogs");
ngx::shared::set(d, "Jim", "8", null, null);
"#,
    );
    let script_q = nginx_quote_path(&script).unwrap();
    write_conf(
        tmp.path(),
        &conf_with_http(
            &module,
            tmp.path(),
            "rss_shared_dict dogs 1m;",
            &format!(
                r#"
        location / {{
            access_by_rss_file {script_q};
            return 200;
        }}
"#
            ),
        ),
    );
    let out = nginx_test(&nginx, tmp.path());
    assert!(
        out.status.success(),
        "shared dict access script should pass nginx -t: {}",
        combined(&out)
    );
}

#[test]
fn rss_shared_dict_nginx_t_rejects_duplicate_name_same_size() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix("shared-dict-dup-same");
    write_conf(
        tmp.path(),
        &conf_with_http(
            &module,
            tmp.path(),
            "rss_shared_dict dogs 1m;\nrss_shared_dict dogs 1m;",
            r#"
        location / {
            return 200;
        }
"#,
        ),
    );
    let out = nginx_test(&nginx, tmp.path());
    assert!(
        !out.status.success(),
        "duplicate zone name must fail nginx -t"
    );
    let text = combined(&out).to_ascii_lowercase();
    assert!(
        text.contains("already") || text.contains("duplicate") || text.contains("rss_shared_dict"),
        "unexpected nginx -t output: {text}"
    );
}

#[test]
fn rss_shared_dict_nginx_t_rejects_duplicate_name_different_size() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix("shared-dict-dup-size");
    write_conf(
        tmp.path(),
        &conf_with_http(
            &module,
            tmp.path(),
            "rss_shared_dict dogs 1m;\nrss_shared_dict dogs 2m;",
            r#"
        location / {
            return 200;
        }
"#,
        ),
    );
    let out = nginx_test(&nginx, tmp.path());
    assert!(
        !out.status.success(),
        "same name different size must fail nginx -t"
    );
}

#[test]
fn rss_shared_dict_nginx_t_rejects_empty_name() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix("shared-dict-empty");
    write_conf(
        tmp.path(),
        &conf_with_http(
            &module,
            tmp.path(),
            "rss_shared_dict \"\" 1m;",
            r#"
        location / {
            return 200;
        }
"#,
        ),
    );
    let out = nginx_test(&nginx, tmp.path());
    assert!(!out.status.success(), "empty zone name must fail nginx -t");
}

#[test]
fn rss_shared_dict_nginx_t_rejects_too_long_name() {
    let nginx = nginx_binary();
    let module = module_cdylib();
    let tmp = isolated_prefix("shared-dict-long");
    let name = "n".repeat(257);
    write_conf(
        tmp.path(),
        &conf_with_http(
            &module,
            tmp.path(),
            &format!("rss_shared_dict {name} 1m;"),
            r#"
        location / {
            return 200;
        }
"#,
        ),
    );
    let out = nginx_test(&nginx, tmp.path());
    assert!(
        !out.status.success(),
        "257-byte zone name must fail nginx -t"
    );
}

#[test]
fn rss_shared_dict_unknown_zone_still_compiles_at_config() {
    compile_rss_source(
        r#"
use ngx;
ngx::shared::dict("missing");
"#,
    )
    .expect("unknown zone name still compiles");
}
