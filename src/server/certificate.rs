use crate::error::{LabyrinthError, Result};
use base64::{engine::general_purpose, Engine as _};
use colored::Colorize;
use ring::digest::{digest, SHA256};
use rustls::pki_types::CertificateDer;
use rustls_pemfile::{certs, pkcs8_private_keys};
use std::fs::File;
use std::io::BufReader;

/// Single Responsibility: Certificate management
pub struct CertificateManager;

impl CertificateManager {
    /// Extract fingerprint from certificate PEM
    pub fn get_fingerprint_from_pem(cert_pem: &str) -> Result<String> {
        let mut cert_bytes = cert_pem.as_bytes();
        let certs = certs(&mut cert_bytes)
            .collect::<std::result::Result<Vec<CertificateDer>, std::io::Error>>()
            .map_err(LabyrinthError::Io)?;

        if let Some(cert) = certs.first() {
            let cert_der = cert.as_ref();
            let hashed_cert = digest(&SHA256, cert_der);
            let fingerprint_hex = hex::encode(hashed_cert.as_ref());
            Ok(fingerprint_hex)
        } else {
            Err(LabyrinthError::Message("No certificate found".to_string()))
        }
    }

    pub fn load_or_generate_cert(
        domain: Option<String>,
    ) -> Result<(
        Vec<CertificateDer<'static>>,
        rustls::pki_types::PrivateKeyDer<'static>,
        String,
    )> {
        // Try to load existing certificate
        if let Ok(cert_pem) = std::fs::read_to_string("cert.pem") {
            if let Ok(_key_pem) = std::fs::read_to_string("key.pem") {
                let cert_file = File::open("cert.pem")?;
                let mut cert_reader = BufReader::new(cert_file);
                let certs = certs(&mut cert_reader)
                    .collect::<std::result::Result<Vec<CertificateDer>, std::io::Error>>()?;

                let key_file = File::open("key.pem")?;
                let mut key_reader = BufReader::new(key_file);
                let mut keys = pkcs8_private_keys(&mut key_reader)
                    .collect::<std::result::Result<Vec<_>, std::io::Error>>()?;

                if !certs.is_empty() && !keys.is_empty() {
                    let key = keys.remove(0);
                    return Ok((certs, key.into(), cert_pem));
                }
            }
        }

        // Generate new certificate
        Self::generate_self_signed_cert(domain)
    }

    fn generate_self_signed_cert(
        domain: Option<String>,
    ) -> Result<(
        Vec<CertificateDer<'static>>,
        rustls::pki_types::PrivateKeyDer<'static>,
        String,
    )> {
        use rcgen::string::Ia5String;
        use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};

        let mut params = CertificateParams::default();
        // Set Common Name
        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, "Labyrinth Server");
        params.distinguished_name = distinguished_name;

        // Add SAN for localhost and optional domain
        let domain_name = domain.unwrap_or_else(|| "localhost".to_string());
        params
            .subject_alt_names
            .push(SanType::DnsName(Ia5String::try_from(domain_name).unwrap()));
        params
            .subject_alt_names
            .push(SanType::DnsName(Ia5String::try_from("localhost").unwrap()));

        // Generate certificate
        let key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
        let cert = params.self_signed(&key_pair)?;
        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();

        // Save to disk
        std::fs::write("cert.pem", &cert_pem)?;
        std::fs::write("key.pem", &key_pem)?;

        // Parse with rustls
        let mut cert_bytes = cert_pem.as_bytes();
        let certs = certs(&mut cert_bytes)
            .collect::<std::result::Result<Vec<CertificateDer>, std::io::Error>>()?;

        let mut key_bytes = key_pem.as_bytes();
        let mut keys = pkcs8_private_keys(&mut key_bytes)
            .collect::<std::result::Result<Vec<_>, std::io::Error>>()?;

        if certs.is_empty() || keys.is_empty() {
            return Err(LabyrinthError::Message(
                "Failed to generate certificate".to_string(),
            ));
        }

        let key = keys.remove(0);
        Ok((certs, key.into(), cert_pem))
    }

    pub fn show_certificate_info() -> Result<()> {
        let cert_pem = std::fs::read_to_string("cert.pem")
            .map_err(|_| LabyrinthError::Message("Certificate file not found".to_string()))?;

        let mut cert_bytes = cert_pem.as_bytes();
        let certs = certs(&mut cert_bytes)
            .collect::<std::result::Result<Vec<CertificateDer>, std::io::Error>>()
            .map_err(LabyrinthError::Io)?;

        if let Some(cert) = certs.first() {
            let cert_der = cert.as_ref();
            let hashed_cert = digest(&SHA256, cert_der);
            let fingerprint_hex = hex::encode(hashed_cert.as_ref());

            // Format fingerprint with colons for better readability
            let formatted_fingerprint = fingerprint_hex
                .chars()
                .collect::<Vec<char>>()
                .chunks(2)
                .map(|chunk| chunk.iter().collect::<String>())
                .collect::<Vec<String>>()
                .join(":");

            // Get base64 certificate and format in chunks
            let base64_cert = general_purpose::STANDARD.encode(cert_pem.as_bytes());
            let formatted_cert = base64_cert
                .chars()
                .collect::<Vec<char>>()
                .chunks(64)
                .map(|chunk| format!("  {}", chunk.iter().collect::<String>()))
                .collect::<Vec<String>>()
                .join("\n");

            println!("\n{}", "Server Certificate Information".cyan().bold());
            println!("{}", "─────────────────────────────".bright_black());
            println!();
            println!("{}", "Fingerprint (SHA-256)".cyan());
            println!("  Readable:     {}", formatted_fingerprint.yellow());
            println!("  Copy-friendly: {}", fingerprint_hex.green().bold());
            println!();
            println!("{}", "Certificate (Base64)".cyan());
            println!("{}", formatted_cert.bright_white());
            println!();
            println!("{}", "─────────────────────────────".bright_black());
        }

        Ok(())
    }
}
