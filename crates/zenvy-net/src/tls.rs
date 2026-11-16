use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

pub struct TlsMaterial {
    pub cert_pem: String,
    pub key_pem: String,
    /// SHA-256 of the DER certificate, uppercase hex with colons.
    pub fingerprint: String,
}

pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(content.as_bytes())
}

/// Loads the per-install self-signed certificate, or creates one. It is regenerated when the set of
/// names/addresses it must cover changes (e.g. the PC's LAN address changed). The private key is
/// stored 0600 and never leaves this machine.
pub fn load_or_create(dir: &Path, sans: &[String]) -> std::io::Result<TlsMaterial> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let (cert_p, key_p, san_p) = (
        dir.join("cert.pem"),
        dir.join("key.pem"),
        dir.join("sans.txt"),
    );
    let mut want: Vec<String> = sans.to_vec();
    want.sort();
    want.dedup();
    let want_txt = want.join("\n");

    if let (Ok(cert), Ok(key), Ok(have)) = (
        std::fs::read_to_string(&cert_p),
        std::fs::read_to_string(&key_p),
        std::fs::read_to_string(&san_p),
    ) {
        if have == want_txt {
            if let Some(der) = pem_to_der(&cert) {
                return Ok(TlsMaterial {
                    fingerprint: fingerprint(&der),
                    cert_pem: cert,
                    key_pem: key,
                });
            }
        }
    }
    let ck = rcgen::generate_simple_self_signed(want.clone()).map_err(std::io::Error::other)?;
    let (cert_pem, key_pem) = (ck.cert.pem(), ck.key_pair.serialize_pem());
    write_private(&key_p, &key_pem)?;
    std::fs::write(&cert_p, &cert_pem)?;
    std::fs::write(&san_p, want_txt)?;
    Ok(TlsMaterial {
        fingerprint: fingerprint(ck.cert.der()),
        cert_pem,
        key_pem,
    })
}

fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    // minimal base64 decode via the auth crate's engine would add a dep; use std-only decoding
    base64_std(&body)
}

fn base64_std(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes().filter(|c| *c != b'=') {
        acc = (acc << 6) | val(c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn created_reused_and_regenerated_on_san_change() {
        let d = tempfile::tempdir().unwrap();
        let a = load_or_create(d.path(), &["192.168.0.10".into(), "localhost".into()]).unwrap();
        assert_eq!(
            std::fs::metadata(d.path().join("key.pem"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let b = load_or_create(d.path(), &["localhost".into(), "192.168.0.10".into()]).unwrap();
        assert_eq!(
            a.fingerprint, b.fingerprint,
            "same names => same cert (stable for pinning)"
        );
        let c = load_or_create(d.path(), &["192.168.0.99".into(), "localhost".into()]).unwrap();
        assert_ne!(a.fingerprint, c.fingerprint, "new address => new cert");
        assert_eq!(a.fingerprint.split(':').count(), 32);
    }
}
