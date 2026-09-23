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
/// `headers`, when given, is a file curl writes the real response headers into. That is where the
/// true size comes from: a separate probe request can be slow or refused (GitHub redirects release
/// downloads to a storage host that has timed out at twenty seconds here) while the download
/// itself is perfectly healthy. The transfer always knows how big the thing it is transferring is.
pub fn download(url: &str, dest: &Path, headers: Option<&Path>) -> Result<(), String> {
    let c = client().ok_or("neither curl nor wget is installed")?;
    let d = dest.to_string_lossy().to_string();
    let st = match c {
        "curl" => {
            let mut c = Command::new("curl");
            c.args(["-fL", "--retry", "5", "--retry-delay", "3", "-C", "-", "-A", UA, "-o", &d, url]);
            if let Some(h) = headers { c.arg("-D").arg(h); }
            // curl's own bar is for a person at a terminal; when something is parsing our
            // output it is just noise on stderr
            if quiet_meter() { c.arg("--no-progress-meter"); } else { c.arg("--progress-bar"); }
            c.status()
        }
        _ => Command::new("wget").args(["-c", "--tries=5", "-U", UA, "-O", &d, url]).status(),
    }.map_err(|e| format!("{c}: {e}"))?;
    if !st.success() { return Err(format!("download failed ({st})")); }
    Ok(())
}

/// True when another program is reading our output and curl should stay quiet.
fn quiet_meter() -> bool { std::env::args().any(|a| a == "--json") }

pub const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) arxburn";

/// Read a total out of response headers: either `Content-Range: bytes 0-0/4100096`, which carries
/// the whole size even for a one byte request, or a plain `Content-Length`. Redirects answer
/// `Content-Length: 0`, so zeros are skipped rather than believed: taking them at face value is
/// what left every progress bar sitting at 0% for the length of the download.
pub fn size_in_headers(head: &str) -> Option<u64> {
    for line in head.lines().rev() {
        let low = line.to_ascii_lowercase();
        if let Some(rest) = low.strip_prefix("content-range:") {
            if let Some(total) = rest.rsplit('/').next() {
                if let Ok(n) = total.trim().parse::<u64>() { if n > 0 { return Some(n); } }
            }
        }
    }
    head.lines().rev()
        .filter(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .filter_map(|l| l.split(':').nth(1))
        .filter_map(|v| v.trim().parse::<u64>().ok())
        .find(|n| *n > 1)
}

/// The size the server reports, so a target can be checked before 4GB is pulled down, and so a
/// progress bar has a denominator.
///
/// This asks for one byte rather than sending a HEAD. A range request answers
/// `Content-Range: bytes 0-0/4100096`, which carries the whole size, and it works on mirrors that
/// refuse HEAD outright. GitHub redirects release downloads to a storage host, and the redirect
/// itself answers `Content-Length: 0`, so the zeros are skipped rather than believed.
pub fn remote_size(url: &str) -> Option<u64> {
    let out = Command::new("curl")
        .args(["-sL", "-r", "0-0", "-o", "/dev/null", "-D", "-",
               "--max-time", "12", "--connect-timeout", "5", "-A", UA, url])
        .output().ok()?;
    size_in_headers(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::size_in_headers;

    #[test]
    fn a_redirects_zero_length_is_never_mistaken_for_the_size() {
        // exactly what github answers for a release download: a 302 with no body, then the file
        let head = "HTTP/2 302\r\ncontent-length: 0\r\n\r\nHTTP/2 206\r\n\
                    content-range: bytes 0-0/4100096\r\ncontent-length: 1\r\n";
        assert_eq!(size_in_headers(head), Some(4_100_096));
    }

    #[test]
    fn a_plain_download_reports_its_content_length() {
        let head = "HTTP/1.1 200 OK\r\nContent-Length: 4223172608\r\n";
        assert_eq!(size_in_headers(head), Some(4_223_172_608));
    }

    #[test]
    fn headers_that_say_nothing_useful_give_no_answer_rather_than_zero() {
        assert_eq!(size_in_headers("HTTP/2 200\r\ncontent-length: 0\r\n"), None);
        assert_eq!(size_in_headers(""), None);
    }
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
mod size_tests {
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
