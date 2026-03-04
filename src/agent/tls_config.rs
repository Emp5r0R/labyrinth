use crate::error::{LabyrinthError, Result};
use crate::styling;
use base64::{engine::general_purpose, Engine as _};
use ring::digest::{digest, SHA256};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::ClientConfig;
use rustls_pemfile::certs;
use std::sync::Arc;
use tracing::{error, info};

/// Single Responsibility: TLS configuration and certificate verification
pub struct TlsConfigManager;

impl TlsConfigManager {
    pub fn create_tls_config(
        server_cert_b64: Option<String>,
        accept_fingerprint: Option<String>,
    ) -> Result<ClientConfig> {
        // Easy certificate handling - prioritize fingerprint, then base64, then file
        let verifier = if let Some(fingerprint) = accept_fingerprint {
            info!("{} Using certificate fingerprint verification", styling::INFO_INDICATOR);
            SmartCertVerifier::from_fingerprint(&fingerprint)?
        } else if let Some(b64_cert) = server_cert_b64 {
            info!("{} Using base64 certificate verification", styling::INFO_INDICATOR);
            SmartCertVerifier::from_cert_b64(&b64_cert)?
        } else {
            info!("{} Using cert.pem file verification", styling::INFO_INDICATOR);
            let cert_pem = std::fs::read_to_string("cert.pem")
                .map_err(|e| LabyrinthError::Message(format!("Failed to read cert.pem: {}. Run server first to generate certificate.", e)))?;
            SmartCertVerifier::from_cert_pem(&cert_pem)?
        };

        let config = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        Ok(config)
    }
}



/// Easy-to-use certificate verifier that works with fingerprints or cert files
#[derive(Debug)]
struct SmartCertVerifier {
    expected_fingerprint: Vec<u8>,
}

impl SmartCertVerifier {
    /// Create verifier from hex fingerprint
    fn from_fingerprint(fingerprint_hex: &str) -> Result<Self> {
        let expected_fingerprint = hex::decode(fingerprint_hex)
            .map_err(|_| LabyrinthError::Message("Invalid fingerprint format".to_string()))?;
        Ok(Self { expected_fingerprint })
    }

    /// Create verifier from certificate PEM string
    fn from_cert_pem(cert_pem: &str) -> Result<Self> {
        let mut cert_bytes = cert_pem.as_bytes();
        let certs = certs(&mut cert_bytes)
            .collect::<std::result::Result<Vec<CertificateDer>, std::io::Error>>()
            .map_err(LabyrinthError::Io)?;
        
        if certs.is_empty() {
            return Err(LabyrinthError::Message("No certificates found in PEM".to_string()));
        }

        let cert_der = certs[0].as_ref();
        let hashed_cert = digest(&SHA256, cert_der);
        let expected_fingerprint = hashed_cert.as_ref().to_vec();
        
        info!("{} Using certificate fingerprint: {}", styling::SUCCESS_INDICATOR, hex::encode(&expected_fingerprint));
        Ok(Self { expected_fingerprint })
    }

    /// Create verifier from base64 encoded certificate
    fn from_cert_b64(cert_b64: &str) -> Result<Self> {
        let cert_bytes = general_purpose::STANDARD.decode(cert_b64)
            .map_err(LabyrinthError::Base64)?;
        let cert_pem = String::from_utf8(cert_bytes)
            .map_err(|_| LabyrinthError::Message("Invalid UTF-8 in certificate".to_string()))?;
        Self::from_cert_pem(&cert_pem)
    }
}

impl ServerCertVerifier for SmartCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer,
        _intermediates: &[CertificateDer],
        _server_name: &ServerName,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let cert_der = end_entity.as_ref();
        let hashed_cert = digest(&SHA256, cert_der);
        let hashed_cert_bytes = hashed_cert.as_ref();

        if hashed_cert_bytes == self.expected_fingerprint.as_slice() {
            info!("{} Certificate fingerprint matches - connection authorized", styling::SUCCESS_INDICATOR);
            Ok(ServerCertVerified::assertion())
        } else {
            error!("{} Certificate fingerprint mismatch. Expected: {}, Got: {}",
                styling::ERROR_INDICATOR,
                hex::encode(&self.expected_fingerprint),
                hex::encode(hashed_cert_bytes)
            );
            Err(rustls::Error::General("Certificate fingerprint mismatch".to_string()))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
        ]
    }
}