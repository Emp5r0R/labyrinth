use crate::error::{LabyrinthError, Result};
use crate::security::{parse_pem_pair, SecurityManager};
use base64::{engine::general_purpose, Engine as _};
use colored::Colorize;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::path::Path;

pub const CERT_FILE: &str = "cert.pem";
pub const KEY_FILE: &str = "key.pem";

/// Server identity loaded from or persisted to disk.
pub type ServerIdentity = (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>, String);

/// Single Responsibility: server certificate persistence and presentation.
pub struct CertificateManager;

impl CertificateManager {
    /// Extract fingerprint from certificate PEM
    pub fn get_fingerprint_from_pem(cert_pem: &str) -> Result<String> {
        SecurityManager::fingerprint_from_pem(cert_pem)
    }

    pub fn load_or_generate_cert(domain: Option<String>) -> Result<ServerIdentity> {
        Self::load_or_generate_cert_in(Path::new("."), domain)
    }

    /// Load `cert.pem`/`key.pem` from `dir`, generating and persisting a new
    /// pair only when either file is absent. Existing but unusable files are an
    /// error: silently replacing them would change the fingerprint that
    /// deployed agents pin.
    pub fn load_or_generate_cert_in(dir: &Path, domain: Option<String>) -> Result<ServerIdentity> {
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);

        if cert_path.exists() && key_path.exists() {
            let cert_pem = std::fs::read_to_string(&cert_path)?;
            let key_pem = std::fs::read_to_string(&key_path)?;
            let (certs, key) = parse_pem_pair(&cert_pem, &key_pem).map_err(|e| {
                LabyrinthError::Message(format!(
                    "Existing {} / {} in {} are unusable ({}); remove them to regenerate",
                    CERT_FILE,
                    KEY_FILE,
                    dir.display(),
                    e
                ))
            })?;
            return Ok((certs, key, cert_pem));
        }

        let domain = domain.unwrap_or_else(|| "localhost".to_string());
        let generated = SecurityManager::generate_certificate("Labyrinth Server", &[&domain])?;
        std::fs::write(&cert_path, &generated.cert_pem)?;
        write_private_key(&key_path, &generated.key_pem)?;

        let (certs, key) = parse_pem_pair(&generated.cert_pem, &generated.key_pem)?;
        Ok((certs, key, generated.cert_pem))
    }

    pub fn show_certificate_info() -> Result<()> {
        let cert_pem = std::fs::read_to_string(CERT_FILE)
            .map_err(|_| LabyrinthError::Message("Certificate file not found".to_string()))?;
        let fingerprint_hex = Self::get_fingerprint_from_pem(&cert_pem)?;
        let base64_cert = general_purpose::STANDARD.encode(cert_pem.as_bytes());

        println!("\n{}", "Server Certificate Information".cyan().bold());
        println!("{}", "─────────────────────────────".bright_black());
        println!();
        println!("{}", "Fingerprint (SHA-256)".cyan());
        println!(
            "  Readable:     {}",
            format_fingerprint(&fingerprint_hex).yellow()
        );
        println!("  Copy-friendly: {}", fingerprint_hex.green().bold());
        println!();
        println!("{}", "Certificate (Base64)".cyan());
        println!("{}", wrap_base64(&base64_cert, 64, "  ").bright_white());
        println!();
        println!("{}", "─────────────────────────────".bright_black());

        Ok(())
    }
}

/// `abcd..` -> `ab:cd:..` for operator display.
pub fn format_fingerprint(hex: &str) -> String {
    hex.as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

pub fn wrap_base64(value: &str, width: usize, indent: &str) -> String {
    value
        .as_bytes()
        .chunks(width.max(1))
        .map(|chunk| format!("{indent}{}", String::from_utf8_lossy(chunk)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn write_private_key(path: &Path, key_pem: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(key_pem.as_bytes())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, key_pem)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::normalize_fingerprint;

    #[test]
    fn generates_persists_and_reloads_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let (certs, _key, pem) =
            CertificateManager::load_or_generate_cert_in(dir.path(), None).unwrap();
        assert_eq!(certs.len(), 1);
        assert!(dir.path().join(CERT_FILE).exists());
        assert!(dir.path().join(KEY_FILE).exists());

        let (reloaded, _key, reloaded_pem) =
            CertificateManager::load_or_generate_cert_in(dir.path(), None).unwrap();
        assert_eq!(pem, reloaded_pem);
        assert_eq!(certs[0].as_ref(), reloaded[0].as_ref());
    }

    #[cfg(unix)]
    #[test]
    fn generated_private_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        CertificateManager::load_or_generate_cert_in(dir.path(), None).unwrap();
        let mode = std::fs::metadata(dir.path().join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn regenerates_when_one_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let (_, _, first) = CertificateManager::load_or_generate_cert_in(dir.path(), None).unwrap();
        std::fs::remove_file(dir.path().join(KEY_FILE)).unwrap();
        let (_, _, second) =
            CertificateManager::load_or_generate_cert_in(dir.path(), None).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn refuses_to_overwrite_corrupt_identity() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CERT_FILE), "not a cert").unwrap();
        std::fs::write(dir.path().join(KEY_FILE), "not a key").unwrap();
        assert!(CertificateManager::load_or_generate_cert_in(dir.path(), None).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(CERT_FILE)).unwrap(),
            "not a cert"
        );
    }

    #[test]
    fn custom_domain_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        assert!(CertificateManager::load_or_generate_cert_in(
            dir.path(),
            Some("c2.example.test".into())
        )
        .is_ok());
    }

    #[test]
    fn fingerprint_matches_security_manager() {
        let generated = SecurityManager::generate_self_signed_certificate("x").unwrap();
        assert_eq!(
            CertificateManager::get_fingerprint_from_pem(&generated.cert_pem).unwrap(),
            SecurityManager::fingerprint_from_pem(&generated.cert_pem).unwrap()
        );
    }

    #[test]
    fn display_fingerprint_is_accepted_back_as_a_pin() {
        let generated = SecurityManager::generate_self_signed_certificate("x").unwrap();
        let hex = CertificateManager::get_fingerprint_from_pem(&generated.cert_pem).unwrap();
        let readable = format_fingerprint(&hex);
        assert_eq!(readable.split(':').count(), 32);
        assert_eq!(
            normalize_fingerprint(&readable).unwrap(),
            normalize_fingerprint(&hex).unwrap()
        );
    }

    #[test]
    fn wrap_base64_chunks_and_indents() {
        assert_eq!(wrap_base64("abcdef", 4, "  "), "  abcd\n  ef");
        assert_eq!(wrap_base64("", 4, "  "), "");
        assert_eq!(wrap_base64("ab", 0, ""), "a\nb");
    }
}
