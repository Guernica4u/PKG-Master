//! Downloads from GitHub releases: the latest release's assets, a file
//! with progress, and unpacking a zip.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::convert::Progress;
use crate::error::{Error, Result};

/// One downloadable file of a release.
#[derive(Clone, Debug)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub tag: String,
    pub assets: Vec<Asset>,
}

const USER_AGENT: &str = concat!("PKG-Master/", env!("CARGO_PKG_VERSION"));

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().user_agent(USER_AGENT).build()
}

fn net_error(what: &str, e: ureq::Error) -> Error {
    Error::Conversion(match e {
        ureq::Error::Status(code, _) => format!("{what}: the server answered {code}"),
        other => format!("{what}: {other}"),
    })
}

/// The latest release of `owner/repo` on GitHub.
pub fn latest_release(repo: &str) -> Result<Release> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let what = format!("Could not check {repo}'s releases");
    let body = agent()
        .get(&url)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| net_error(&what, e))?
        .into_string()?;
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| Error::Conversion(format!("{what}: {e}")))?;
    let tag = v.get("tag_name").and_then(|t| t.as_str()).unwrap_or_default().to_string();
    let assets = v
        .get("assets")
        .and_then(|a| a.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    Some(Asset {
                        name: a.get("name")?.as_str()?.to_string(),
                        url: a.get("browser_download_url")?.as_str()?.to_string(),
                        size: a.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release { tag, assets })
}

/// Download `asset` to `dest`. It is written to `dest.part` first and only
/// renamed once complete, so a cancelled download never looks finished.
pub fn download(asset: &Asset, dest: &Path, progress: Progress) -> Result<()> {
    let what = format!("Could not download {}", asset.name);
    let resp = agent().get(&asset.url).call().map_err(|e| net_error(&what, e))?;
    let total = resp.header("Content-Length").and_then(|l| l.parse().ok()).unwrap_or(asset.size);
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let result = (|| -> Result<()> {
        let mut reader = resp.into_reader();
        let mut out = fs::File::create(&part)?;
        let mut buf = vec![0u8; 1 << 16];
        let mut done = 0u64;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, total, &format!("Downloading {}", asset.name));
        }
        out.sync_all()?;
        if total > 0 && done != total {
            return Err(Error::Conversion(format!("{what}: got {done} of {total} bytes")));
        }
        fs::rename(&part, dest)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// Unpack `zip` into `dest`. Entries that would land outside `dest` are refused.
pub fn unzip(zip: &Path, dest: &Path, progress: Progress) -> Result<()> {
    let bad = |e: zip::result::ZipError| Error::Conversion(format!("{} is not a usable zip: {e}", zip.display()));
    let mut archive = zip::ZipArchive::new(fs::File::open(zip)?).map_err(bad)?;
    let total = archive.len() as u64;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(bad)?;
        let Some(rel) = entry.enclosed_name() else {
            return Err(Error::Conversion(format!("{} has an unsafe path: {}", zip.display(), entry.name())));
        };
        let out = dest.join(rel);
        progress(i as u64 + 1, total, &format!("Unpacking {}", entry.name()));
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut file)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut w = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, data) in entries {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn unzips_into_folders() {
        let dir = std::env::temp_dir().join(format!("pkgmaster-unzip-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("a.zip");
        make_zip(&zip, &[("prx-windows/libs/libc.prx", b"x"), ("prx-windows/USAGE.md", b"hi")]);
        let out = dir.join("out");
        unzip(&zip, &out, &|_, _, _| {}).unwrap();
        assert_eq!(fs::read(out.join("prx-windows/libs/libc.prx")).unwrap(), b"x");
        assert_eq!(fs::read(out.join("prx-windows/USAGE.md")).unwrap(), b"hi");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_paths_outside() {
        let dir = std::env::temp_dir().join(format!("pkgmaster-unzip-bad-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("bad.zip");
        make_zip(&zip, &[("../escaped.txt", b"no")]);
        assert!(unzip(&zip, &dir.join("out"), &|_, _, _| {}).is_err());
        assert!(!dir.join("escaped.txt").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
