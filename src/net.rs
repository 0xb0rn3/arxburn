//! Talking to the network, through curl (or wget).
//!
//! arxburn has no dependencies, and std has no TLS, so the transport is the HTTP client every
//! machine already has. That is not a compromise: curl handles redirects, resume, proxies and
//! certificate stores better than anything that would fit in this repo, and it means a download
//! interrupted at 3GB continues instead of starting over.

use std::path::Path;
use std::process::{Command, Stdio};

fn client() -> Option<&'static str> {
    for c in ["curl", "wget"] {
        if Command::new("sh").arg("-c").arg(format!("command -v {c}"))
            .stdout(Stdio::null()).status().map(|s| s.success()).unwrap_or(false) { return Some(c); }
    }
    None
}

/// GET a page or index as text. Small, bounded, used to resolve "latest".
pub fn text(url: &str) -> Result<String, String> {
    let c = client().ok_or("neither curl nor wget is installed")?;
    let out = match c {
        "curl" => Command::new("curl")
            .args(["-fsSL", "--max-time", "60", "--retry", "2", "-A", UA, url]).output(),
        _ => Command::new("wget").args(["-q", "-O", "-", "--timeout=60", "-U", UA, url]).output(),
    }.map_err(|e| format!("{c}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{c} could not read {url} ({})", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// GET with extra headers, for APIs that insist on a referer or a session cookie.
pub fn text_with(url: &str, headers: &[String]) -> Result<String, String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-fsSL", "--max-time", "60", "-A", UA]);
    for h in headers { cmd.arg("-H").arg(h); }
    cmd.arg(url);
    let out = cmd.output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() { return Err(format!("curl failed on {url} ({})", out.status)); }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Download to a file, resuming a partial one, with the client's own progress meter on the
/// terminal. A 5GB image over a bad line is exactly the case where resume matters.
pub fn download(url: &str, dest: &Path) -> Result<(), String> {
    let c = client().ok_or("neither curl nor wget is installed")?;
    let d = dest.to_string_lossy().to_string();
    let st = match c {
        "curl" => Command::new("curl")
            .args(["-fL", "--retry", "5", "--retry-delay", "3", "-C", "-", "--progress-bar",
                   "-A", UA, "-o", &d, url]).status(),
        _ => Command::new("wget").args(["-c", "--tries=5", "-U", UA, "-O", &d, url]).status(),
    }.map_err(|e| format!("{c}: {e}"))?;
    if !st.success() { return Err(format!("download failed ({st})")); }
    Ok(())
}

pub const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) arxburn";

/// The size the server reports, so a target can be checked before 4GB is pulled down.
pub fn remote_size(url: &str) -> Option<u64> {
    let out = Command::new("curl").args(["-fsIL", "--max-time", "30", "-A", UA, url]).output().ok()?;
    let head = String::from_utf8_lossy(&out.stdout);
    head.lines().rev()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
}

/// Pull the href targets out of an HTML index or a plain listing.
pub fn hrefs(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = body.as_bytes();
    let mut i = 0;
    while let Some(p) = find(b, i, b"href=") {
        let mut j = p + 5;
        let quote = if j < b.len() && (b[j] == b'"' || b[j] == b'\'') { let q = b[j]; j += 1; Some(q) } else { None };
        let start = j;
        while j < b.len() {
            match quote {
                Some(q) if b[j] == q => break,
                None if b[j] == b'>' || b[j].is_ascii_whitespace() => break,
                _ => j += 1,
            }
        }
        if j > start {
            if let Ok(s) = std::str::from_utf8(&b[start..j]) { out.push(s.to_string()); }
        }
        i = j.max(p + 5);
    }
    out
}

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= hay.len() { return None; }
    hay[from..].windows(needle.len()).position(|w| w.eq_ignore_ascii_case(needle)).map(|p| p + from)
}

/// Compare names the way a human reads versions: 24.10 is newer than 9.10, and
/// 2026.09.01 is newer than 2026.08.31. A plain string compare gets both wrong.
pub fn natural_newer(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut x, mut y) = (a.bytes().peekable(), b.bytes().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(p), Some(q)) => {
                if p.is_ascii_digit() && q.is_ascii_digit() {
                    let mut np = 0u64; let mut nq = 0u64;
                    while let Some(d) = x.peek().copied().filter(u8::is_ascii_digit) { np = np.saturating_mul(10) + (d - b'0') as u64; x.next(); }
                    while let Some(d) = y.peek().copied().filter(u8::is_ascii_digit) { nq = nq.saturating_mul(10) + (d - b'0') as u64; y.next(); }
                    match np.cmp(&nq) { Ordering::Equal => continue, o => return o }
                }
                match p.cmp(&q) { Ordering::Equal => { x.next(); y.next(); }, o => return o }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn versions_sort_like_versions_not_like_strings() {
        assert_eq!(natural_newer("24.10", "9.10"), Ordering::Greater);
        assert_eq!(natural_newer("2026.09.01", "2026.08.31"), Ordering::Greater);
        assert_eq!(natural_newer("Fedora-43", "Fedora-9"), Ordering::Greater);
        assert_eq!(natural_newer("1.2.3", "1.2.3"), Ordering::Equal);
    }

    #[test]
    fn hrefs_come_out_of_real_index_pages() {
        let page = r#"<a href="../">..</a><a href='archlinux-2026.09.01-x86_64.iso'>iso</a>
                      <a href=plain.iso>x</a>"#;
        let h = hrefs(page);
        assert!(h.contains(&"archlinux-2026.09.01-x86_64.iso".to_string()));
        assert!(h.contains(&"plain.iso".to_string()));
    }
}

/// Follow the redirects and report where a link actually lands. Microsoft's fwlink ids, and
/// plenty of "download latest" links, only reveal the filename this way.
pub fn final_url(url: &str) -> Option<String> {
    let out = Command::new("curl")
        .args(["-fsIL", "--max-time", "45", "-A", UA, "-o", "/dev/null", "-w", "%{url_effective}", url])
        .output().ok()?;
    let u = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && u.starts_with("http") { Some(u) } else { None }
}
