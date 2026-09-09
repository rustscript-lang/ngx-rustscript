//! Tokio-owned outbound sockets.
//!
//! Host wrappers live in `src/host/socket.rs`. UDP and TCP drivers own
//! generation-safe operation identity, DNS, and timeouts.

pub(crate) mod pool;
pub(crate) mod tcp;
pub(crate) mod tls;
pub(crate) mod udp;
