//! `devil registry serve` — a static, read-only registry.
//!
//! It is deliberately dumb: a directory tree, served over HTTP. That makes it
//! usable as a private mirror, an air-gapped registry or a test fixture.
//!
//! ```text
//! <dir>/
//!   index/<name>.json          the PackageIndex document
//!   dl/<name>/<version>        the .devil archive
//! ```
//!
//! `devil pack` plus `devil registry index` populate that layout.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::ui;
use devil_core::util::checksum;
use devil_registry::{IndexEntry, PackageIndex};

#[derive(Debug, clap::Args)]
pub struct Serve {
    /// Directory holding `index/` and `dl/`.
    #[arg(long, default_value = "registry")]
    pub dir: PathBuf,
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub addr: String,
}

#[derive(Debug, clap::Args)]
pub struct Index {
    /// `.devil` archives to index.
    #[arg(required = true)]
    pub packages: Vec<PathBuf>,
    /// Registry directory to write into.
    #[arg(long, default_value = "registry")]
    pub dir: PathBuf,
}

/// Serve until interrupted.
pub fn serve(args: Serve) -> Result<()> {
    let root = std::fs::canonicalize(&args.dir)
        .map_err(|e| anyhow::anyhow!("{}: {e}", args.dir.display()))?;
    let listener = TcpListener::bind(&args.addr)?;
    let local = listener.local_addr()?;
    ui::ok(format!("serving {} on http://{local}", root.display()));
    println!("  press Ctrl+C to stop");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = handle(stream, &root) {
                    eprintln!("{} {e}", ui::yellow("warning:"));
                }
            }
            Err(e) => eprintln!("{} {e}", ui::yellow("warning:")),
        }
    }
    Ok(())
}

fn handle(mut stream: TcpStream, root: &Path) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    // Drain the headers; nothing here needs them.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
    }

    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let path = path.split('?').next().unwrap_or("/");
    let decoded = percent_decode(path);

    match resolve(root, &decoded) {
        Some(file) if file.is_file() => {
            let mut buf = Vec::new();
            std::fs::File::open(&file)?.read_to_end(&mut buf)?;
            let content_type = if decoded.ends_with(".json") {
                "application/json"
            } else {
                "application/gzip"
            };
            write_response(&mut stream, 200, content_type, &buf)?;
        }
        _ => {
            let body = format!("not found: {decoded}");
            write_response(&mut stream, 404, "text/plain", body.as_bytes())?;
        }
    }
    Ok(())
}

/// Map a request path to a file inside the registry directory.
fn resolve(root: &Path, request: &str) -> Option<PathBuf> {
    let trimmed = request.trim_start_matches('/');
    if trimmed.is_empty() {
        return Some(root.to_path_buf());
    }
    // Only these two prefixes are exposed; everything else is refused so the
    // server can never leak files outside the registry layout.
    let mut parts = trimmed.split('/');
    let first = parts.next()?;
    let rest: Vec<&str> = parts.collect();
    let candidate = match (first, rest.as_slice()) {
        ("index", [name]) => root.join("index").join(format!("{name}.json")),
        ("dl", [name, version]) => root.join("dl").join(name).join(version),
        ("index", []) | ("dl", []) => return Some(root.join(first)),
        _ => return None,
    };
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(candidate)
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: public, max-age=60\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Build `index/<name>.json` and `dl/<name>/<version>` from `.devil` archives.
pub fn index(args: Index) -> Result<()> {
    for archive in &args.packages {
        let meta = devil_package::read_meta(archive)
            .map_err(|e| anyhow::anyhow!("{}: {e}", archive.display()))?;
        let bytes = std::fs::read(archive)?;
        let sum = checksum(&bytes);

        let dl = args.dir.join("dl").join(&meta.name).join(&meta.version);
        std::fs::create_dir_all(&dl)?;
        let target = dl.join(format!("{}-{}.devil", meta.name, meta.version));
        std::fs::copy(archive, &target)?;

        let index_dir = args.dir.join("index");
        std::fs::create_dir_all(&index_dir)?;
        let index_path = index_dir.join(format!("{}.json", meta.name));

        let mut index = if index_path.is_file() {
            let text = std::fs::read_to_string(&index_path)?;
            serde_json::from_str::<PackageIndex>(&text).unwrap_or_else(|_| PackageIndex::empty(&meta.name))
        } else {
            PackageIndex::empty(&meta.name)
        };
        index.name = meta.name.clone();
        index.versions.insert(
            meta.version.clone(),
            IndexEntry {
                name: meta.name.clone(),
                version: meta.version.clone(),
                cksum: sum,
                deps: meta.dependencies.clone(),
                features: meta.features.clone(),
                yanked: false,
                size: bytes.len() as u64,
                published_at: None,
                downloads: 0,
                edition: Some(meta.edition.clone()),
                description: meta.description.clone(),
                license: meta.license.clone(),
                repository: meta.repository.clone(),
                keywords: meta.keywords.clone(),
            },
        );

        std::fs::write(
            &index_path,
            serde_json::to_string_pretty(&index).unwrap_or_default(),
        )?;
        ui::ok(format!("indexed {}@{}", meta.name, meta.version));
    }
    Ok(())
}