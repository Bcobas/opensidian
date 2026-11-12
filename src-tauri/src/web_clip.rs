// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use dom_query::Document;
use sha2::{Digest, Sha256};
use url::Url;

pub const FOLDER: &str = "Web captures";
const MAX_HTML: u64 = 4 * 1024 * 1024;
const MAX_IMAGE: u64 = 8 * 1024 * 1024;
const MAX_IMAGES: usize = 16;
const MAX_ASSETS: usize = 32 * 1024 * 1024;

pub struct Capture {
    pub markdown: String,
    pub assets: BTreeMap<String, Vec<u8>>,
    pub omitted: usize,
}

fn web_url(url: Url) -> Option<Url> {
    (matches!(url.scheme(), "http" | "https") && url.username().is_empty() && url.password().is_none()
        && url.port().is_none() && url.host_str().is_some()).then_some(url)
}

fn public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !ip.is_private() && !ip.is_loopback() && !ip.is_link_local() && !ip.is_documentation()
        && !matches!((a, b, c), (0, _, _) | (100, 64..=127, _) | (192, 0, 0) | (198, 18..=19, _) | (224..=255, _, _))
}

fn fetch(mut url: Url, limit: u64) -> Result<(Url, Vec<u8>), String> {
    #[cfg(windows)]
    let curl = PathBuf::from(std::env::var_os("SystemRoot").ok_or("Windows system folder unavailable")?)
        .join("System32").join("curl.exe");
    #[cfg(not(windows))]
    let curl = PathBuf::from("/usr/bin/curl");

    for _ in 0..5 {
        let host = url.host_str().ok_or("Missing website host")?;
        let port = url.port_or_known_default().ok_or("Unsupported website port")?;
        let ip = (host, port).to_socket_addrs().map_err(|_| "Could not resolve the website host")?
            .find_map(|address| match address.ip() { IpAddr::V4(ip) if public_ipv4(ip) => Some(ip), _ => None })
            .ok_or("The website needs a public IPv4 address")?;
        // Bind this request to the vetted address. Each redirect is validated separately.
        let resolve = format!("{host}:{port}:{ip}");
        let mut command = Command::new(&curl);
        command.args(["--disable", "--noproxy", "*", "--globoff", "--proto", "=http,https", "--silent", "--include",
            "--connect-timeout", "6", "--max-time", "15", "--max-filesize", &limit.to_string(), "--resolve", &resolve, "--", url.as_str()]);
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().map_err(|e| format!("Could not start curl. {e}"))?;
        let mut bytes = Vec::new();
        let read = child.stdout.take().expect("piped download").take(limit + 65_537).read_to_end(&mut bytes);
        if read.is_err() || bytes.len() as u64 > limit + 65_536 { let _ = child.kill(); }
        let status = child.wait().map_err(|e| e.to_string())?;
        read.map_err(|e| e.to_string())?;
        if !status.success() { return Err("Website download failed or exceeded its size or time limit".to_owned()); }
        let offset = bytes.windows(4).position(|part| part == b"\r\n\r\n").ok_or("Invalid HTTP response")?;
        if offset > 65_536 || bytes.len() - offset - 4 > limit as usize { return Err("Website response is too large".to_owned()); }
        let header = String::from_utf8_lossy(&bytes[..offset]);
        let mut lines = header.lines();
        let code: u16 = lines.next().and_then(|line| line.split_whitespace().nth(1)).and_then(|code| code.parse().ok())
            .ok_or("Invalid HTTP response")?;
        match code {
            200..=299 => return Ok((url, bytes.split_off(offset + 4))),
            301 | 302 | 303 | 307 | 308 => {
                let location = lines.find_map(|line| line.split_once(':').filter(|(key, _)| key.eq_ignore_ascii_case("location")))
                    .map(|(_, location)| location.trim()).ok_or("Redirect has no location")?;
                url = url.join(location).ok().and_then(web_url).ok_or("Unsupported redirect URL")?;
            }
            _ => return Err(format!("Website returned HTTP {code}")),
        }
    }
    Err("Too many website redirects".to_owned())
}

fn image_extension(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") { Some("png") }
    else if data.starts_with(b"\xff\xd8\xff") { Some("jpg") }
    else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") { Some("gif") }
    else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP".as_slice()) { Some("webp") }
    else { None }
}

fn markdown_url(url: &Url) -> String { url.as_str().replace('(', "%28").replace(')', "%29") }

fn capture_html<F>(html: &str, source: &Url, keep_images: bool, mut image_bytes: F) -> Result<Capture, String>
where F: FnMut(Url) -> Result<Vec<u8>, String> {
    let doc = Document::from(html);
    doc.select("script,style,iframe,nav,header,footer,form,button,svg,template,noscript,aside,[hidden]").remove();
    let selection = ["article", "main", "[role=main]", "body"].into_iter()
        .map(|selector| doc.select_single(selector)).find(|selection| !selection.is_empty())
        .ok_or("No readable page content")?;
    let content = selection.nodes().first().ok_or("No readable page content")?;
    for link in selection.select("a[href]").nodes() {
        if let Some(url) = link.attr("href").and_then(|href| source.join(&href).ok()).and_then(web_url) {
            link.set_attr("href", &markdown_url(&url));
        } else {
            link.remove_attr("href");
        }
    }
    let mut assets = BTreeMap::new();
    let mut sources: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut omitted = 0;
    let mut size = 0;
    for image in selection.select("img").nodes() {
        if !keep_images { image.remove_from_parent(); continue; }
        let target = image.attr("src").and_then(|href| source.join(&href).ok()).and_then(web_url);
        let local = target.and_then(|url| {
            let key = url.to_string();
            if let Some(cached) = sources.get(&key) { return cached.clone(); }
            if sources.len() >= MAX_IMAGES || size >= MAX_ASSETS { return None; }
            let result = image_bytes(url).ok().and_then(|data| {
                let ext = image_extension(&data)?;
                if size + data.len() > MAX_ASSETS { return None; }
                let file = format!("{:x}.{ext}", Sha256::digest(&data));
                size += data.len();
                assets.insert(file.clone(), data);
                Some(file)
            });
            sources.insert(key, result.clone());
            result
        });
        if let Some(file) = local { image.set_attr("src", &format!("Assets/{file}")); }
        else { omitted += 1; image.remove_from_parent(); }
    }
    let markdown = content.md(Some(&[]));
    if markdown.trim().is_empty() { return Err("No readable text or images found".to_owned()); }
    let markdown = format!("Source: <{}>\n\n{}\n", markdown_url(source), markdown.trim());
    if markdown.len() as u64 > crate::index::MAX_NOTE_BYTES { return Err("Captured note is too large".to_owned()); }
    Ok(Capture { markdown, assets, omitted })
}

pub fn capture(source: &str, fragment: Option<&str>, images: bool) -> Result<Capture, String> {
    let source = Url::parse(source.trim()).ok().and_then(web_url).ok_or("Enter a valid HTTP or HTTPS source URL")?;
    let (source, html) = if let Some(fragment) = fragment.filter(|html| !html.trim().is_empty()) {
        if fragment.len() as u64 > MAX_HTML { return Err("Pasted fragment is too large".to_owned()); }
        (source, fragment.to_owned())
    } else {
        let (source, bytes) = fetch(source, MAX_HTML)?;
        (source, String::from_utf8_lossy(&bytes).into_owned())
    };
    capture_html(&html, &source, images, |url| fetch(url, MAX_IMAGE).map(|(_, data)| data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_a_fragment_with_source_links_and_only_requested_images() {
        let url = Url::parse("https://example.com/reports/index.html").unwrap();
        let html = r#"<nav>Menu</nav><article><h1>Report</h1><p><a href="details.html">Details</a> <a href="javascript:void(0)">Plain</a></p><img src="chart.png" alt="Chart"><img src="data:image/png,example"></article>"#;
        let omitted = capture_html(html, &url, false, |_| panic!("unexpected image request")).unwrap();
        assert!(!omitted.markdown.contains("Assets/"));
        let capture = capture_html(html, &url, true, |image| {
            assert_eq!(image.as_str(), "https://example.com/reports/chart.png");
            Ok(b"\x89PNG\r\n\x1a\nfixture".to_vec())
        }).unwrap();
        assert_eq!(capture.omitted, 1);
        assert_eq!(capture.assets.len(), 1);
        assert!(capture.markdown.contains("Source: <https://example.com/reports/index.html>"));
        assert!(capture.markdown.contains("https://example.com/reports/details.html"));
        assert!(capture.markdown.contains("![Chart](Assets/"));
        assert!(!capture.markdown.contains("javascript:"));
        assert!(!capture.markdown.contains("Menu"));
        for ip in ["127.0.0.1", "10.1.2.3", "169.254.169.254", "100.64.0.1"] {
            assert!(!public_ipv4(ip.parse().unwrap()));
        }
    }
}
