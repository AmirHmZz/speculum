use crate::error::{Error, Result};
use std::collections::BTreeMap;

/// Paths remain relative to the archive root, including signed and compressed indexes.
pub fn classify(path: &str) -> Result<(bool, bool, BTreeMap<String, String>)> {
    if !path.starts_with("dists/") && !path.starts_with("pool/") {
        return Err(Error::not_found());
    }
    let mut checksums = BTreeMap::new();
    if let Some((_, suffix)) = path.rsplit_once("/by-hash/") {
        let (algorithm, digest) = suffix
            .split_once('/')
            .ok_or_else(|| Error::bad("Invalid by-hash path"))?;
        let (name, length) = match algorithm {
            "SHA256" => ("sha256", 64),
            "SHA512" => ("sha512", 128),
            _ => return Err(Error::bad("Unsupported by-hash algorithm")),
        };
        if digest.len() != length || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::bad("Invalid by-hash digest"));
        }
        checksums.insert(name.into(), digest.into());
    }
    Ok((
        path.starts_with("pool/") || !checksums.is_empty(),
        !path.starts_with("pool/"),
        checksums,
    ))
}
