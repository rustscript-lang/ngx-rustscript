fn main() {
    println!("cargo::rerun-if-changed=stubs.c");
    cc::Build::new()
        .file("stubs.c")
        .warnings(true)
        .compile("ngx_rustscript_nginx_test_stubs");
}
