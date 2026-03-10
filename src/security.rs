use crate::error::{LabyrinthError, Result};
use base64::{engine::general_purpose, Engine as _};
use ring::digest::{digest, SHA256};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::ClientConfig;
use rustls_pemfile::certs;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct GeneratedCertificate {
    pub cert_pem: String,
    pub key_pem: String,
}

pub struct SecurityManager;

impl SecurityManager {
    pub fn create_tls_client_config(
        server_cert_b64: Option<String>,
        accept_fingerprint: Option<String>,
    ) -> Result<ClientConfig> {
        let verifier = if let Some(fingerprint) = accept_fingerprint {
            FingerprintVerifier::from_fingerprint(&fingerprint)?
        } else if let Some(b64_cert) = server_cert_b64 {
            FingerprintVerifier::from_cert_b64(&b64_cert)?
        } else {
            let cert_pem = std::fs::read_to_string("cert.pem").map_err(|e| {
                LabyrinthError::Message(format!(
                    "Failed to read cert.pem: {}. Run server first to generate certificate.",
                    e
                ))
            })?;
            FingerprintVerifier::from_cert_pem(&cert_pem)?
        };

        Ok(ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth())
    }

    pub fn fingerprint_from_pem(cert_pem: &str) -> Result<String> {
        let mut cert_bytes = cert_pem.as_bytes();
        let parsed = certs(&mut cert_bytes)
            .collect::<std::result::Result<Vec<CertificateDer>, std::io::Error>>()
            .map_err(LabyrinthError::Io)?;

        let cert = parsed
            .first()
            .ok_or_else(|| LabyrinthError::Message("No certificate found".to_string()))?;

        Ok(hex::encode(digest(&SHA256, cert.as_ref()).as_ref()))
    }

    pub fn generate_self_signed_certificate(common_name: &str) -> Result<GeneratedCertificate> {
        use rcgen::string::Ia5String;
        use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};

        let mut params = CertificateParams::default();
        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, common_name);
        params.distinguished_name = distinguished_name;
        params.subject_alt_names.push(SanType::DnsName(
            Ia5String::try_from("localhost".to_string()).unwrap(),
        ));

        let key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
        let cert = params.self_signed(&key_pair)?;
        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();
        let _ = Self::fingerprint_from_pem(&cert_pem)?;

        Ok(GeneratedCertificate { cert_pem, key_pem })
    }
}

#[derive(Debug)]
pub struct FingerprintVerifier {
    expected_fingerprint: Vec<u8>,
}

impl FingerprintVerifier {
    pub fn from_fingerprint(fingerprint_hex: &str) -> Result<Self> {
        let expected_fingerprint = hex::decode(fingerprint_hex)
            .map_err(|_| LabyrinthError::Message("Invalid fingerprint format".to_string()))?;
        Ok(Self {
            expected_fingerprint,
        })
    }

    pub fn from_cert_pem(cert_pem: &str) -> Result<Self> {
        let fingerprint = SecurityManager::fingerprint_from_pem(cert_pem)?;
        Self::from_fingerprint(&fingerprint)
    }

    pub fn from_cert_b64(cert_b64: &str) -> Result<Self> {
        let cert_bytes = general_purpose::STANDARD
            .decode(cert_b64)
            .map_err(LabyrinthError::Base64)?;
        let cert_pem = String::from_utf8(cert_bytes)
            .map_err(|_| LabyrinthError::Message("Invalid UTF-8 in certificate".to_string()))?;
        Self::from_cert_pem(&cert_pem)
    }
}

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer,
        _intermediates: &[CertificateDer],
        _server_name: &ServerName,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let hashed = digest(&SHA256, end_entity.as_ref());
        if hashed.as_ref() == self.expected_fingerprint.as_slice() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "Certificate fingerprint mismatch".to_string(),
            ))
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
