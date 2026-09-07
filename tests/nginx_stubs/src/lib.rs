//! Native NGINX C ABI stubs linked only into `cargo test` binaries.
//!
//! Production `libngx_rustscript.so` leaves these symbols undefined so the
//! nginx process provides them. Every stub returns NULL / `NGX_ERROR` and
//! cannot be mistaken for a successful NGINX operation.

/// Ensure the static stub archive is pulled into the test link line.
pub fn force_link() {}
