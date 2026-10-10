//! Small helpers shared across crates: checksums, URL joining, name validation.

use sha2::{Digest, Sha256};

/// `sha256-<hex>`, the checksum format used by the registry.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn checksum(bytes: &[u8]) -> String {
    format!("sha256-{}", sha256_hex(bytes))
}

/// Join a registry base URL with a relative path without doubling slashes.
pub fn join_url(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

/// Valid package-name rule, deliberately narrower than npm's: lowercase, no
/// scopes, ASCII only. Same shape as Rust crate names.
pub const NAME_RULE: &str = "lowercase ascii letters, digits, `-`, `_`, starting with a letter";

pub fn validate_name(name: &str) -> crate::Result<()> {
    let reason = |r: &str| {
        crate::error::Error::InvalidName {
            name: name.to_string(),
            reason: r.to_string(),
            pattern: NAME_RULE,
        }
    };

    if name.is_empty() {
        return Err(reason("is empty"));
    }
    if name.len() > 64 {
        return Err(reason("is longer than 64 characters"));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return Err(reason("must start with a lowercase letter"));
    }
    for ch in name.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_';
        if !ok {
            return Err(reason(&format!("contains disallowed character `{ch}`")));
        }
    }
    Ok(())
}

/// Split a CLI argument like `chalk@1.2.0` or `chalk@^1`.
pub fn split_name_version(spec: &str) -> (String, Option<String>) {
    match spec.rfind('@') {
        // `a@1.0` -> name `a`, version `1.0`
        Some(idx) if idx > 0 => (spec[..idx].to_string(), Some(spec[idx + 1..].to_string())),
        _ => (spec.to_string(), None),
    }
}

/// Human readable byte size.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// Replace path separators so a name/version pair is a single directory name.
pub fn sanitize_segment(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            other => other,
        })
        .collect()
}