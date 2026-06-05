//! TLS configuration: turning the `--tls` / `--ca-cert` / `--tls-no-verify` flags
//! into a transport setup for the channel.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::DigitallySignedStruct;
use rustls::SignatureScheme;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint};

/// How the channel should be secured, derived from the TLS flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TlsMode {
    /// Plaintext `http://`; TLS is off.
    Disabled,
    /// TLS with the platform's trusted roots.
    SystemRoots,
    /// TLS verifying the server against a private CA from a PEM file.
    CustomCa(PathBuf),
    /// TLS with certificate verification disabled (debugging only).
    NoVerify,
}

impl TlsMode {
    /// Whether TLS is on; drives the `https` scheme.
    pub(crate) fn is_enabled(&self) -> bool {
        !matches!(self, TlsMode::Disabled)
    }
}

/// A TLS setup failure: reading the CA file or applying the config to the channel.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TlsError {
    #[error("failed to read CA certificate {path}: {source}")]
    ReadCa {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("TLS configuration failed: {0}")]
    Config(#[source] tonic::transport::Error),
}

/// Reads a PEM CA certificate from disk.
fn load_ca_certificate(path: &Path) -> Result<Certificate, TlsError> {
    let pem = std::fs::read(path).map_err(|source| TlsError::ReadCa {
        path: path.display().to_string(),
        source,
    })?;
    Ok(Certificate::from_pem(pem))
}

/// Applies the TLS mode to the endpoint. `host` is the SNI/authority name.
pub(crate) fn configure(
    endpoint: Endpoint,
    mode: &TlsMode,
    host: &str,
) -> Result<Endpoint, TlsError> {
    match mode {
        TlsMode::Disabled => Ok(endpoint),
        TlsMode::SystemRoots => {
            let config = ClientTlsConfig::new().with_native_roots().domain_name(host);
            endpoint.tls_config(config).map_err(TlsError::Config)
        }
        TlsMode::CustomCa(path) => {
            let ca = load_ca_certificate(path)?;
            let config = ClientTlsConfig::new().ca_certificate(ca).domain_name(host);
            endpoint.tls_config(config).map_err(TlsError::Config)
        }
        TlsMode::NoVerify => {
            let config = ClientTlsConfig::new().domain_name(host);
            endpoint
                .tls_config_with_verifier(config, Arc::new(NoVerify))
                .map_err(TlsError::Config)
        }
    }
}

/// A server certificate verifier that accepts any certificate.
/// Used only for `--tls-no-verify`, which the help text marks as dangerous:
/// it removes all protection against man-in-the-middle attacks.
#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
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
    fn is_enabled_only_when_tls_is_on() {
        assert!(!TlsMode::Disabled.is_enabled());
        assert!(TlsMode::SystemRoots.is_enabled());
        assert!(TlsMode::CustomCa("/tmp/ca.pem".into()).is_enabled());
        assert!(TlsMode::NoVerify.is_enabled());
    }

    #[test]
    fn load_ca_reads_pem_bytes() {
        let dir = std::env::temp_dir();
        let path = dir.join("grpcknock-load-ca-test.pem");
        std::fs::write(&path, b"-----BEGIN CERTIFICATE-----\nabc\n").unwrap();

        let cert = load_ca_certificate(&path).unwrap();
        assert_eq!(cert.get_ref(), b"-----BEGIN CERTIFICATE-----\nabc\n");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_ca_missing_file_errors() {
        let path = Path::new("/nonexistent/grpcknock/ca.pem");
        assert!(matches!(
            load_ca_certificate(path),
            Err(TlsError::ReadCa { .. })
        ));
    }

    #[test]
    fn no_verify_accepts_any_certificate() {
        let verifier = NoVerify;
        let cert = CertificateDer::from(vec![0x30, 0x00]);
        let server_name = ServerName::try_from("example.test").unwrap();
        let result = verifier.verify_server_cert(
            &cert,
            &[],
            &server_name,
            &[],
            UnixTime::since_unix_epoch(std::time::Duration::from_secs(0)),
        );
        assert!(result.is_ok());
    }
}
