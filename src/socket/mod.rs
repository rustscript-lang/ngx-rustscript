//! Tokio-owned outbound sockets.
//!
//! Host wrappers live in `src/host/socket.rs`. This module owns the UDP
//! driver, generation-safe operation identity, DNS, and timeouts.

pub(crate) mod udp;
