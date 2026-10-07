use crate::error::{LabyrinthError, Result};
use base64::{engine::general_purpose, Engine as _};
use ring::digest::{digest, SHA256};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::ClientConfig;
use rustls_pemfile::certs;
use std::path::Path;
use std::sync::Arc;

/// SHA-256 output length; a pinned fingerprint of any other size can never match.
pub const FINGERPRINT_LEN: usize = 32;
/// Fallback trust anchor used when neither a fingerprint nor a certificate is supplied.
pub const DEFAULT_CERT_PATH: &str = "cert.pem";

#[derive(Debug, Clone)]
pub struct GeneratedCertificate {
    pub cert_pem: String,
    pub key_pem: String,
}

/// Constant-time comparison for shared secrets. Both sides are hashed first so
/// neither the content nor the length of the expected key leaks through timing.
pub fn keys_match(provided: &str, expected: &str) -> bool {
    let provided = digest(&SHA256, provided.as_bytes());
    let expected = digest(&SHA256, expected.as_bytes());
    provided
        .as_ref()
        .iter()
        .zip(expected.as_ref())
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

/// Parse a PEM certificate chain and its private key (PKCS#8, PKCS#1 or SEC1).
pub fn parse_pem_pair(
    cert_pem: &str,
    key_pem: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let chain = certs(&mut cert_pem.as_bytes()).collect::<std::result::Result<Vec<_>, _>>()?;
    if chain.is_empty() {
        return Err(LabyrinthError::Message(
            "No certificate found in PEM".to_string(),
        ));
    }
    let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())?
        .ok_or_else(|| LabyrinthError::Message("No private key found in PEM".to_string()))?;
    Ok((chain, key))
}

pub struct SecurityManager;

impl SecurityManager {
    /// Build a client config pinned to the server certificate. Precedence:
    /// explicit fingerprint, then base64 certificate, then `cert.pem` in the
    /// working directory.
    pub fn create_tls_client_config(
        server_cert_b64: Option<String>,
        accept_fingerprint: Option<String>,
    ) -> Result<ClientConfig> {
        let verifier = FingerprintVerifier::resolve(
            server_cert_b64.as_deref(),
            accept_fingerprint.as_deref(),
            Path::new(DEFAULT_CERT_PATH),
        )?;
        Ok(Self::client_config_with_verifier(verifier))
    }

    pub fn client_config_with_verifier(verifier: FingerprintVerifier) -> ClientConfig {
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth()
    }

    pub fn fingerprint_from_pem(cert_pem: &str) -> Result<String> {
        let mut cert_bytes = cert_pem.as_bytes();
        let parsed = certs(&mut cert_bytes)
            .collect::<std::result::Result<Vec<CertificateDer>, std::io::Error>>()
            .map_err(LabyrinthError::Io)?;

        let cert = parsed
            .first()
            .ok_or_else(|| LabyrinthError::Message("No certificate found".to_string()))?;

        Ok(fingerprint_der(cert))
    }

    pub fn generate_self_signed_certificate(common_name: &str) -> Result<GeneratedCertificate> {
        Self::generate_certificate(common_name, &["localhost"])
    }

    /// Generate an ECDSA P-256 self-signed certificate. `localhost` is always
    /// included because agents default their SNI to it.
    pub fn generate_certificate(
        common_name: &str,
        dns_names: &[&str],
    ) -> Result<GeneratedCertificate> {
        use rcgen::string::Ia5String;
        use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};

        let mut params = CertificateParams::default();
        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, common_name);
        params.distinguished_name = distinguished_name;

        let mut names: Vec<&str> = Vec::with_capacity(dns_names.len() + 1);
        for name in dns_names.iter().copied().chain(["localhost"]) {
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
        for name in names {
            let name = Ia5String::try_from(name.to_string()).map_err(|e| {
                LabyrinthError::Message(format!("Invalid certificate DNS name `{name}`: {e}"))
            })?;
            params.subject_alt_names.push(SanType::DnsName(name));
        }

        let key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
        let cert = params.self_signed(&key_pair)?;
        Ok(GeneratedCertificate {
            cert_pem: cert.pem(),
            key_pem: key_pair.serialize_pem(),
        })
    }
}

pub fn fingerprint_der(cert: &CertificateDer<'_>) -> String {
    hex::encode(digest(&SHA256, cert.as_ref()).as_ref())
}

/// Accept fingerprints as printed by the `cert` command: plain hex, or
/// colon/space separated, in either case.
pub fn normalize_fingerprint(input: &str) -> Result<Vec<u8>> {
    let cleaned: String = input
        .chars()
        .filter(|ch| !matches!(ch, ':' | '-') && !ch.is_whitespace())
        .collect();
    let bytes = hex::decode(&cleaned)
        .map_err(|_| LabyrinthError::Message("Invalid fingerprint format".to_string()))?;
    if bytes.len() != FINGERPRINT_LEN {
        return Err(LabyrinthError::Message(format!(
            "Invalid fingerprint length: expected {} bytes of SHA-256, got {}",
            FINGERPRINT_LEN,
            bytes.len()
        )));
    }
    Ok(bytes)
}

/// Pins the server's leaf certificate by SHA-256 and still verifies that the
/// peer proves possession of that certificate's private key.
#[derive(Debug)]
pub struct FingerprintVerifier {
    expected_fingerprint: Vec<u8>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl FingerprintVerifier {
    pub fn from_fingerprint(fingerprint_hex: &str) -> Result<Self> {
        Ok(Self {
            expected_fingerprint: normalize_fingerprint(fingerprint_hex)?,
            algorithms: rustls::crypto::ring::default_provider().signature_verification_algorithms,
        })
    }

    pub fn from_cert_pem(cert_pem: &str) -> Result<Self> {
        let fingerprint = SecurityManager::fingerprint_from_pem(cert_pem)?;
        Self::from_fingerprint(&fingerprint)
    }

    pub fn from_cert_b64(cert_b64: &str) -> Result<Self> {
        let cert_bytes = general_purpose::STANDARD
            .decode(cert_b64.trim())
            .map_err(LabyrinthError::Base64)?;
        let cert_pem = String::from_utf8(cert_bytes)
            .map_err(|_| LabyrinthError::Message("Invalid UTF-8 in certificate".to_string()))?;
        Self::from_cert_pem(&cert_pem)
    }

    /// Resolve the trust anchor with the same precedence as the CLI.
    pub fn resolve(
        server_cert_b64: Option<&str>,
        accept_fingerprint: Option<&str>,
        fallback_cert_path: &Path,
    ) -> Result<Self> {
        if let Some(fingerprint) = accept_fingerprint {
            return Self::from_fingerprint(fingerprint);
        }
        if let Some(cert_b64) = server_cert_b64 {
            return Self::from_cert_b64(cert_b64);
        }
        let cert_pem = std::fs::read_to_string(fallback_cert_path).map_err(|e| {
            LabyrinthError::Message(format!(
                "Failed to read {}: {}. Run server first to generate certificate.",
                fallback_cert_path.display(),
                e
            ))
        })?;
        Self::from_cert_pem(&cert_pem)
    }

    pub fn expected_fingerprint_hex(&self) -> String {
        hex::encode(&self.expected_fingerprint)
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

    // The certificate is public, so pinning alone is not enough: the handshake
    // signature proves the peer holds the matching private key.
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn generated() -> GeneratedCertificate {
        SecurityManager::generate_self_signed_certificate("labyrinth-test").unwrap()
    }

    #[test]
    fn keys_match_is_exact() {
        assert!(keys_match("secret", "secret"));
        assert!(!keys_match("secret", "Secret"));
        assert!(!keys_match("secret", "secret "));
        assert!(!keys_match("", "secret"));
        assert!(keys_match("", ""));
    }

    #[test]
    fn generated_certificate_round_trips_through_pem_parsing() {
        let cert = generated();
        let (chain, _key) = parse_pem_pair(&cert.cert_pem, &cert.key_pem).unwrap();
        assert_eq!(chain.len(), 1);
        let fingerprint = SecurityManager::fingerprint_from_pem(&cert.cert_pem).unwrap();
        assert_eq!(fingerprint.len(), FINGERPRINT_LEN * 2);
        assert_eq!(fingerprint, fingerprint_der(&chain[0]));
    }

    #[test]
    fn generated_certificates_are_unique() {
        let a = SecurityManager::fingerprint_from_pem(&generated().cert_pem).unwrap();
        let b = SecurityManager::fingerprint_from_pem(&generated().cert_pem).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn generate_certificate_rejects_invalid_dns_name() {
        assert!(SecurityManager::generate_certificate("cn", &["bad name\u{7f}é"]).is_err());
    }

    #[test]
    fn parse_pem_pair_rejects_missing_parts() {
        let cert = generated();
        assert!(parse_pem_pair("", &cert.key_pem).is_err());
        assert!(parse_pem_pair(&cert.cert_pem, "").is_err());
        assert!(parse_pem_pair("garbage", "garbage").is_err());
    }

    #[test]
    fn fingerprint_from_pem_rejects_non_certificates() {
        assert!(SecurityManager::fingerprint_from_pem("").is_err());
        assert!(SecurityManager::fingerprint_from_pem(&generated().key_pem).is_err());
    }

    #[test]
    fn normalize_fingerprint_accepts_display_formats() {
        let hex = "ab".repeat(FINGERPRINT_LEN);
        let colon = vec!["AB"; FINGERPRINT_LEN].join(":");
        let spaced = vec!["ab"; FINGERPRINT_LEN].join(" ");
        let expected = vec![0xab; FINGERPRINT_LEN];
        assert_eq!(normalize_fingerprint(&hex).unwrap(), expected);
        assert_eq!(normalize_fingerprint(&colon).unwrap(), expected);
        assert_eq!(normalize_fingerprint(&spaced).unwrap(), expected);
        assert_eq!(
            normalize_fingerprint(&format!("  {hex}\n")).unwrap(),
            expected
        );
    }

    #[test]
    fn normalize_fingerprint_rejects_bad_hex_and_wrong_length() {
        // Regression: an empty pin decoded to zero bytes and was accepted.
        assert!(normalize_fingerprint("").is_err());
        assert!(normalize_fingerprint("zz").is_err());
        assert!(normalize_fingerprint(&"ab".repeat(FINGERPRINT_LEN - 1)).is_err());
        assert!(normalize_fingerprint(&"ab".repeat(FINGERPRINT_LEN + 1)).is_err());
        assert!(normalize_fingerprint(&"a".repeat(FINGERPRINT_LEN * 2 - 1)).is_err());
    }

    #[test]
    fn verifier_from_cert_b64_and_pem_agree() {
        let cert = generated();
        let b64 = general_purpose::STANDARD.encode(cert.cert_pem.as_bytes());
        let from_b64 = FingerprintVerifier::from_cert_b64(&b64).unwrap();
        let from_pem = FingerprintVerifier::from_cert_pem(&cert.cert_pem).unwrap();
        assert_eq!(
            from_b64.expected_fingerprint_hex(),
            from_pem.expected_fingerprint_hex()
        );
        assert!(FingerprintVerifier::from_cert_b64(&format!(" {b64}\n")).is_ok());
    }

    #[test]
    fn verifier_rejects_bad_base64_and_non_utf8() {
        assert!(FingerprintVerifier::from_cert_b64("%%%").is_err());
        let not_utf8 = general_purpose::STANDARD.encode([0xff, 0xfe, 0xfd]);
        assert!(FingerprintVerifier::from_cert_b64(&not_utf8).is_err());
    }

    #[test]
    fn resolve_prefers_fingerprint_then_cert_then_file() {
        let pinned = generated();
        let other = generated();
        let pinned_fp = SecurityManager::fingerprint_from_pem(&pinned.cert_pem).unwrap();
        let other_b64 = general_purpose::STANDARD.encode(other.cert_pem.as_bytes());
        let missing = Path::new("/nonexistent/labyrinth/cert.pem");

        let verifier =
            FingerprintVerifier::resolve(Some(&other_b64), Some(&pinned_fp), missing).unwrap();
        assert_eq!(verifier.expected_fingerprint_hex(), pinned_fp);

        let verifier = FingerprintVerifier::resolve(Some(&other_b64), None, missing).unwrap();
        assert_eq!(
            verifier.expected_fingerprint_hex(),
            SecurityManager::fingerprint_from_pem(&other.cert_pem).unwrap()
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cert.pem");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(pinned.cert_pem.as_bytes())
            .unwrap();
        let verifier = FingerprintVerifier::resolve(None, None, &path).unwrap();
        assert_eq!(verifier.expected_fingerprint_hex(), pinned_fp);

        let error = FingerprintVerifier::resolve(None, None, missing).unwrap_err();
        assert!(error.to_string().contains("Run server first"));
    }

    #[test]
    fn verify_server_cert_matches_only_pinned_certificate() {
        let pinned = generated();
        let other = generated();
        let verifier = FingerprintVerifier::from_cert_pem(&pinned.cert_pem).unwrap();
        let (pinned_chain, _) = parse_pem_pair(&pinned.cert_pem, &pinned.key_pem).unwrap();
        let (other_chain, _) = parse_pem_pair(&other.cert_pem, &other.key_pem).unwrap();
        let name = ServerName::try_from("localhost").unwrap();
        let now = UnixTime::now();

        assert!(verifier
            .verify_server_cert(&pinned_chain[0], &[], &name, &[], now)
            .is_ok());
        assert!(verifier
            .verify_server_cert(&other_chain[0], &[], &name, &[], now)
            .is_err());
    }

    #[test]
    fn verifier_advertises_modern_signature_schemes() {
        let verifier = FingerprintVerifier::from_cert_pem(&generated().cert_pem).unwrap();
        let schemes = verifier.supported_verify_schemes();
        assert!(schemes.contains(&rustls::SignatureScheme::ECDSA_NISTP256_SHA256));
        assert!(schemes.contains(&rustls::SignatureScheme::ED25519));
        assert!(schemes.contains(&rustls::SignatureScheme::RSA_PSS_SHA256));
    }

    #[test]
    fn create_tls_client_config_rejects_invalid_pin() {
        assert!(SecurityManager::create_tls_client_config(None, Some("nothex".into())).is_err());
    }
}
