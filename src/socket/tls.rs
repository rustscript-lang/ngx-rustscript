//! rustls client handshake for `ngx.socket.tcp`.
//!
//! Trusted-store and client-cert material live in typed VM-owned resources.
//! `TcpInner` holds only `Arc` handles to those resources — never PEM or a
//! raw private key. Pool identity uses SHA-256 of public certificate DERs
//! (never private keys) plus SNI and verify. Resource id/generation still
//! detect closed credentials.
//!
//! rustls 0.23 always sends an OCSP `status_request` in ClientHello and has
//! no public API to disable it. `send_status_req=true`/`null` is therefore
//! effective; `false` fails closed. rustls also has no script-facing session
//! userdata type: `reused_session` session handles are rejected, while
//! `null`/`true` keep automatic same-identity resumption and `false`
//! disables it.

use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore, SignatureScheme,
};
use tokio_rustls::TlsConnector;
use vm::resource::{CloseProgress, HostResource};
use vm::{ResourceCloseReason, ResourceHandle, ResourceTypeKey, Value, Vm, VmError, VmResult};

use super::pool::{self, MAX_PEM_BYTES, PoolEndpoint, PoolKey, TlsIdentity, insert_pooled};
use super::tcp::{
    self, TcpHandshakeOpContext, TcpInner, fail_closed, inner_from_sock, install_pooled,
    install_pooled_io_only, lock, take_pooled_io,
};
use crate::host::{
    NGX_SOCKET_TLS_CLIENTCERT_RESOURCE_KEY, NGX_SOCKET_TLS_TRUSTEDSTORE_RESOURCE_KEY,
};

static NEXT_CRED_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static SESSION_STORE: Arc<rustls::client::ClientSessionMemoryCache> =
        Arc::new(rustls::client::ClientSessionMemoryCache::new(256));
}

fn next_cred_id() -> u64 {
    NEXT_CRED_ID.fetch_add(1, Ordering::Relaxed)
}

pub(crate) struct TrustedStore {
    inner: Arc<TrustedStoreInner>,
}

pub(crate) struct TrustedStoreInner {
    id: u64,
    generation: AtomicU64,
    fingerprint: [u8; 32],
    roots: Mutex<Option<Arc<RootCertStore>>>,
}

pub(crate) struct ClientCert {
    inner: Arc<ClientCertInner>,
}

pub(crate) struct ClientCertInner {
    id: u64,
    generation: AtomicU64,
    fingerprint: [u8; 32],
    material: Mutex<Option<ClientMaterial>>,
}

struct ClientMaterial {
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

impl Drop for ClientMaterial {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.key);
    }
}

impl TrustedStoreInner {
    fn fingerprint(&self) -> VmResult<[u8; 32]> {
        if lock(&self.roots).is_none() {
            return Err(VmError::HostError(
                "ngx::socket::tcp::sslhandshake trusted store is closed".into(),
            ));
        }
        let _ = self.id;
        let _ = self.generation.load(Ordering::SeqCst);
        Ok(self.fingerprint)
    }

    fn roots(&self) -> VmResult<Arc<RootCertStore>> {
        lock(&self.roots).clone().ok_or_else(|| {
            VmError::HostError("ngx::socket::tcp::sslhandshake trusted store is closed".into())
        })
    }
}

impl ClientCertInner {
    fn fingerprint(&self) -> VmResult<[u8; 32]> {
        if lock(&self.material).is_none() {
            return Err(VmError::HostError(
                "ngx::socket::tcp::sslhandshake client certificate is closed".into(),
            ));
        }
        let _ = self.id;
        let _ = self.generation.load(Ordering::SeqCst);
        Ok(self.fingerprint)
    }

    fn material(&self) -> VmResult<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
        let guard = lock(&self.material);
        let material = guard.as_ref().ok_or_else(|| {
            VmError::HostError("ngx::socket::tcp::sslhandshake client certificate is closed".into())
        })?;
        Ok((material.certs.clone(), material.key.clone_key()))
    }
}

impl HostResource for TrustedStore {
    fn resource_type_key() -> Option<ResourceTypeKey>
    where
        Self: Sized,
    {
        Some(
            ResourceTypeKey::new(NGX_SOCKET_TLS_TRUSTEDSTORE_RESOURCE_KEY)
                .expect("ngx.socket.tls.trustedstore is a valid resource type key"),
        )
    }

    fn begin_close(
        &mut self,
        _reason: ResourceCloseReason,
    ) -> vm::resource::ResourceResult<CloseProgress> {
        *lock(&self.inner.roots) = None;
        self.inner.generation.fetch_add(1, Ordering::SeqCst);
        Ok(CloseProgress::Ready)
    }
}

impl HostResource for ClientCert {
    fn resource_type_key() -> Option<ResourceTypeKey>
    where
        Self: Sized,
    {
        Some(
            ResourceTypeKey::new(NGX_SOCKET_TLS_CLIENTCERT_RESOURCE_KEY)
                .expect("ngx.socket.tls.clientcert is a valid resource type key"),
        )
    }

    fn begin_close(
        &mut self,
        _reason: ResourceCloseReason,
    ) -> vm::resource::ResourceResult<CloseProgress> {
        *lock(&self.inner.material) = None;
        self.inner.generation.fetch_add(1, Ordering::SeqCst);
        Ok(CloseProgress::Ready)
    }
}

fn host_ctx(error: vm::HostContextError) -> VmError {
    VmError::HostError(error.to_string())
}

fn parse_resource_handle(handle_id: i64, name: &str) -> VmResult<ResourceHandle> {
    if handle_id <= 0 {
        return Err(VmError::HostError(format!(
            "{name} invalid resource handle {handle_id}"
        )));
    }
    ResourceHandle::from_raw(handle_id as u64).map_err(|error| {
        VmError::HostError(format!(
            "{name} invalid resource handle {handle_id}: {error}"
        ))
    })
}

fn pem_bytes(data: &Value, name: &str) -> VmResult<zeroize::Zeroizing<Vec<u8>>> {
    let bytes = tcp::payload_bytes(data, name)?;
    if bytes.len() > MAX_PEM_BYTES {
        return Err(VmError::HostError(format!(
            "{name} PEM exceeds the {MAX_PEM_BYTES}-byte limit"
        )));
    }
    Ok(zeroize::Zeroizing::new(bytes))
}

fn parse_certs(pem: &Value, name: &str) -> VmResult<Vec<CertificateDer<'static>>> {
    let bytes = pem_bytes(pem, name)?;
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut io::Cursor::new(bytes.as_slice()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                VmError::HostError(format!("{name} invalid certificate PEM: {error}"))
            })?;
    if certs.is_empty() {
        return Err(VmError::HostError(format!(
            "{name} PEM contained no certificates"
        )));
    }
    Ok(certs)
}

fn parse_private_key(pem: &Value, name: &str) -> VmResult<PrivateKeyDer<'static>> {
    let bytes = pem_bytes(pem, name)?;
    rustls_pemfile::private_key(&mut io::Cursor::new(bytes.as_slice()))
        .map_err(|error| VmError::HostError(format!("{name} invalid private key PEM: {error}")))?
        .ok_or_else(|| VmError::HostError(format!("{name} PEM contained no private key")))
}

fn certs_fingerprint(certs: &[CertificateDer<'static>]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for cert in certs {
        hasher.update((cert.len() as u32).to_be_bytes());
        hasher.update(cert.as_ref());
    }
    hasher.finalize().into()
}

fn ensure_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn validate_client_pair(
    certs: &[CertificateDer<'static>],
    key: &PrivateKeyDer<'static>,
    name: &str,
) -> VmResult<()> {
    ensure_provider();
    ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_client_auth_cert(certs.to_vec(), key.clone_key())
        .map_err(|_| {
            VmError::HostError(format!(
                "{name} client certificate does not match private key"
            ))
        })?;
    Ok(())
}

pub(crate) fn trustedstore_from_pem(pem: Value) -> VmResult<TrustedStore> {
    let name = "ngx::socket::tcp::trustedstore";
    let certs = parse_certs(&pem, name)?;
    let mut roots = RootCertStore::empty();
    for cert in &certs {
        roots
            .add(cert.clone())
            .map_err(|error| VmError::HostError(format!("{name} invalid trust anchor: {error}")))?;
    }
    Ok(TrustedStore {
        inner: Arc::new(TrustedStoreInner {
            id: next_cred_id(),
            generation: AtomicU64::new(1),
            fingerprint: certs_fingerprint(&certs),
            roots: Mutex::new(Some(Arc::new(roots))),
        }),
    })
}

pub(crate) fn clientcert_from_pem(cert: Value, pkey: Value) -> VmResult<ClientCert> {
    let name = "ngx::socket::tcp::clientcert";
    let certs = parse_certs(&cert, name)?;
    let key = parse_private_key(&pkey, name)?;
    validate_client_pair(&certs, &key, name)?;
    Ok(ClientCert {
        inner: Arc::new(ClientCertInner {
            id: next_cred_id(),
            generation: AtomicU64::new(1),
            fingerprint: certs_fingerprint(&certs),
            material: Mutex::new(Some(ClientMaterial { certs, key })),
        }),
    })
}

pub(crate) fn settrustedstore(vm: &mut Vm, sock: i64, store: i64) -> VmResult<()> {
    let name = "ngx::socket::tcp::settrustedstore";
    let handle = parse_resource_handle(store, name)?;
    let attached = {
        let ctx = vm.host_context();
        let resource = ctx
            .borrow_resource::<TrustedStore>(handle)
            .map_err(host_ctx)?;
        Arc::clone(&resource.get().inner)
    };
    let inner = inner_from_sock(vm, sock, name)?;
    *lock(&inner.trusted_store) = Some(attached);
    Ok(())
}

pub(crate) fn setclientcert(vm: &mut Vm, sock: i64, cert: i64) -> VmResult<()> {
    let name = "ngx::socket::tcp::setclientcert";
    let handle = parse_resource_handle(cert, name)?;
    let attached = {
        let ctx = vm.host_context();
        let resource = ctx
            .borrow_resource::<ClientCert>(handle)
            .map_err(host_ctx)?;
        Arc::clone(&resource.get().inner)
    };
    let inner = inner_from_sock(vm, sock, name)?;
    *lock(&inner.client_cert) = Some(attached);
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionFlag {
    Auto,
    Disabled,
}

fn parse_reused_session(value: &Value, name: &str) -> VmResult<SessionFlag> {
    match value {
        Value::Null | Value::Bool(true) => Ok(SessionFlag::Auto),
        Value::Bool(false) => Ok(SessionFlag::Disabled),
        Value::Int(_) => Err(VmError::HostError(format!(
            "{name} reused_session session handles are unsupported; pass null for automatic same-identity resumption or false to disable"
        ))),
        _ => Err(VmError::HostError(format!(
            "{name} reused_session expects null, true, or false"
        ))),
    }
}

fn parse_send_status_req(value: &Value, name: &str) -> VmResult<()> {
    match value {
        Value::Null | Value::Bool(true) => Ok(()),
        Value::Bool(false) => Err(VmError::HostError(format!(
            "{name} send_status_req=false is unsupported; rustls 0.23 always sends an OCSP status_request"
        ))),
        _ => Err(VmError::HostError(format!(
            "{name} send_status_req expects null or true"
        ))),
    }
}

fn parse_verify(value: &Value, name: &str) -> VmResult<bool> {
    match value {
        Value::Null => Ok(true),
        Value::Bool(flag) => Ok(*flag),
        _ => Err(VmError::HostError(format!(
            "{name} ssl_verify expects bool or null"
        ))),
    }
}

fn parse_server_name(value: &Value, name: &str) -> VmResult<String> {
    match value {
        Value::Null => Ok(String::new()),
        Value::String(text) => Ok(text.to_string()),
        _ => Err(VmError::HostError(format!(
            "{name} server_name expects string or null"
        ))),
    }
}

fn snapshot_identity(inner: &TcpInner, sni: String, verify: bool) -> VmResult<TlsIdentity> {
    let trust_fp = match lock(&inner.trusted_store).as_ref() {
        Some(store) => Some(store.fingerprint()?),
        None => None,
    };
    let client_fp = match lock(&inner.client_cert).as_ref() {
        Some(cert) => Some(cert.fingerprint()?),
        None => None,
    };
    Ok(TlsIdentity {
        sni,
        verify,
        trust_fp,
        client_fp,
    })
}

fn park_current_if_tls(inner: &TcpInner) -> VmResult<()> {
    let Some(old) = lock(&inner.tls).clone() else {
        return Ok(());
    };
    let endpoint = lock(&inner.endpoint).clone().ok_or_else(|| {
        VmError::HostError("ngx::socket::tcp::sslhandshake socket is not connected".into())
    })?;
    match take_pooled_io(inner) {
        Ok(io) => {
            let reused = inner.reused_times.load(Ordering::SeqCst);
            insert_pooled(
                PoolKey {
                    endpoint,
                    tls: Some(old),
                },
                io,
                reused,
                None,
                pool::DEFAULT_POOL_SIZE,
            );
            *lock(&inner.tls) = None;
            Ok(())
        }
        Err(error) => {
            if lock(&inner.io).is_none() {
                fail_closed(inner);
            }
            Err(error)
        }
    }
}

fn try_checkout_matching_tls(
    inner: &TcpInner,
    endpoint: &PoolEndpoint,
    wanted: &TlsIdentity,
) -> VmResult<bool> {
    let Some(conn) = pool::checkout_matching_tls(endpoint, wanted) else {
        return Ok(false);
    };
    if lock(&inner.io).is_some() {
        match take_pooled_io(inner) {
            Ok(plain) => {
                insert_pooled(
                    PoolKey {
                        endpoint: endpoint.clone(),
                        tls: None,
                    },
                    plain,
                    inner.reused_times.load(Ordering::SeqCst),
                    None,
                    pool::DEFAULT_POOL_SIZE,
                );
            }
            Err(error) => {
                insert_pooled(
                    PoolKey {
                        endpoint: endpoint.clone(),
                        tls: Some(wanted.clone()),
                    },
                    conn.io,
                    conn.reused_times.saturating_sub(1),
                    None,
                    pool::DEFAULT_POOL_SIZE,
                );
                if lock(&inner.io).is_none() {
                    fail_closed(inner);
                }
                return Err(error);
            }
        }
    }
    install_pooled(inner, conn, endpoint.clone());
    *lock(&inner.tls) = Some(wanted.clone());
    Ok(true)
}

pub(crate) async fn sslhandshake(
    ctx: &TcpHandshakeOpContext,
    reused_session: Value,
    server_name: Value,
    ssl_verify: Value,
    send_status_req: Value,
) -> VmResult<()> {
    let name = "ngx::socket::tcp::sslhandshake";
    ctx.check_identity(name)?;
    let session_flag = parse_reused_session(&reused_session, name)?;
    parse_send_status_req(&send_status_req, name)?;
    let verify = parse_verify(&ssl_verify, name)?;
    let sni = parse_server_name(&server_name, name)?;
    let wanted = snapshot_identity(&ctx.inner, sni.clone(), verify)?;
    if lock(&ctx.inner.tls).as_ref() == Some(&wanted) {
        return Ok(());
    }

    let endpoint = lock(&ctx.inner.endpoint)
        .clone()
        .ok_or_else(|| VmError::HostError(format!("{name} socket is not connected")))?;

    if lock(&ctx.inner.tls).is_some() {
        park_current_if_tls(&ctx.inner)?;
        ctx.check_identity(name)?;
    }

    if try_checkout_matching_tls(&ctx.inner, &endpoint, &wanted)? {
        return Ok(());
    }

    let io = match take_pooled_io(&ctx.inner) {
        Ok(io) => io,
        Err(error) => {
            if lock(&ctx.inner.io).is_none() {
                fail_closed(&ctx.inner);
            }
            return Err(error);
        }
    };

    let config = match client_config(&ctx.inner, verify, session_flag) {
        Ok(config) => config,
        Err(error) => {
            install_pooled_io_only(&ctx.inner, io);
            return Err(error);
        }
    };
    let connector = TlsConnector::from(config);
    let server = if sni.is_empty() {
        match &endpoint {
            PoolEndpoint::Tcp { host, .. } => host.clone(),
            PoolEndpoint::Unix { .. } => {
                install_pooled_io_only(&ctx.inner, io);
                return Err(VmError::HostError(format!(
                    "{name} unix sockets require an explicit server_name"
                )));
            }
        }
    } else {
        sni
    };
    let dns_name = match ServerName::try_from(server.clone()) {
        Ok(name) => name,
        Err(_) => {
            install_pooled_io_only(&ctx.inner, io);
            return Err(VmError::HostError(format!(
                "{name} invalid server_name {server}"
            )));
        }
    };

    let handshake = async move {
        match io {
            pool::PooledIo::Tcp(stream) => connector
                .connect(dns_name, stream)
                .await
                .map(pool::PooledIo::TlsTcp),
            pool::PooledIo::Unix(stream) => connector
                .connect(dns_name, stream)
                .await
                .map(pool::PooledIo::TlsUnix),
            pool::PooledIo::TlsTcp(_) | pool::PooledIo::TlsUnix(_) => {
                Err(io::Error::other("already a TLS stream"))
            }
        }
    };

    let tls_io = match tcp::with_timeout(&ctx.inner, ctx.generation, name, handshake).await {
        Ok(io) => io,
        Err(error) => {
            fail_closed(&ctx.inner);
            return Err(error);
        }
    };
    ctx.check_identity(name)?;
    install_pooled_io_only(&ctx.inner, tls_io);
    *lock(&ctx.inner.tls) = Some(wanted);
    Ok(())
}

fn client_config(
    inner: &TcpInner,
    verify: bool,
    session: SessionFlag,
) -> VmResult<Arc<ClientConfig>> {
    ensure_provider();
    let builder = ClientConfig::builder();
    let builder = if verify {
        let roots = match lock(&inner.trusted_store).as_ref() {
            Some(store) => store.roots()?,
            None => Arc::new(system_or_webpki_roots()?),
        };
        builder.with_root_certificates((*roots).clone())
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
    };
    let mut config = if let Some(client) = lock(&inner.client_cert).as_ref() {
        let (certs, key) = client.material()?;
        builder.with_client_auth_cert(certs, key).map_err(|error| {
            VmError::HostError(format!(
                "ngx::socket::tcp::sslhandshake invalid client certificate: {error}"
            ))
        })?
    } else {
        builder.with_no_client_auth()
    };
    config.resumption = match session {
        SessionFlag::Auto => SESSION_STORE.with(|store| {
            rustls::client::Resumption::store(
                Arc::clone(store) as Arc<dyn rustls::client::ClientSessionStore>
            )
        }),
        SessionFlag::Disabled => rustls::client::Resumption::disabled(),
    };
    Ok(Arc::new(config))
}

fn system_or_webpki_roots() -> VmResult<RootCertStore> {
    let mut roots = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    if !native.certs.is_empty() {
        for cert in native.certs {
            let _ = roots.add(cert);
        }
    } else {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    if roots.is_empty() {
        return Err(VmError::HostError(
            "ngx::socket::tcp::sslhandshake no trust anchors available".into(),
        ));
    }
    Ok(roots)
}

#[derive(Debug)]
struct NoVerifier;

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reused_session_rejects_handles_and_honors_bool() {
        let name = "ngx::socket::tcp::sslhandshake";
        assert_eq!(
            parse_reused_session(&Value::Null, name).unwrap(),
            SessionFlag::Auto
        );
        assert_eq!(
            parse_reused_session(&Value::Bool(true), name).unwrap(),
            SessionFlag::Auto
        );
        assert_eq!(
            parse_reused_session(&Value::Bool(false), name).unwrap(),
            SessionFlag::Disabled
        );
        let err = parse_reused_session(&Value::Int(7), name).unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }

    #[test]
    fn send_status_req_false_is_unsupported() {
        let name = "ngx::socket::tcp::sslhandshake";
        parse_send_status_req(&Value::Null, name).unwrap();
        parse_send_status_req(&Value::Bool(true), name).unwrap();
        let err = parse_send_status_req(&Value::Bool(false), name).unwrap_err();
        assert!(err.to_string().contains("unsupported"));
        assert!(err.to_string().contains("OCSP"));
    }
}
