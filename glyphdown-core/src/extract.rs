//! extract — Read section-extraction (F1, internal-ref). For large file-read tool
//! output, keep a structural outline + every load-bearing (anchor) line + a head
//! preview, and replace each contiguous run of dropped body lines with a single
//! marker carrying the exact retrieval range. The full original is stashed in the
//! `rewind` store first, so every dropped byte is recoverable by id+range —
//! lossy-but-recoverable, never lossy.
//!
//! Validation (bench corpus): 12/13 reads are >1K tokens and pass through
//! ~uncompressed today; 6/12 aggressive reads drop load-bearing anchors, which is
//! why anchor lines are kept verbatim and the rest is rewind-backed.

use crate::{anchor, codec, rewind};

/// Minimum payload size before extraction is worth its marker overhead.
const MIN_BYTES: usize = 1500;
/// Head lines always kept verbatim (cheap orientation for the model).
const HEAD_LINES: usize = 12;

pub struct Extracted {
    pub text: String,
    pub rewind_id: String,
    pub original_tokens: i64,
    pub extracted_tokens: i64,
    pub dropped_lines: usize,
}

/// A line is kept verbatim if it is a structural landmark — headings, code item
/// declarations, list/JSON keys — so the model keeps the shape of the file.
fn is_structural(line: &str) -> bool {
    let t = line.trim_start();
    if t.is_empty() {
        return false;
    }
    // markdown / outline headings, list markers, table rules
    if t.starts_with('#') || t.starts_with("- ") || t.starts_with("* ") || t.starts_with("|") {
        return true;
    }
    // code item declarations across common languages
    const DECL: &[&str] = &[
        "def ",
        "class ",
        "fn ",
        "pub fn",
        "pub(crate) fn",
        "async fn",
        "function ",
        "impl ",
        "struct ",
        "enum ",
        "trait ",
        "interface ",
        "type ",
        "func ",
        "public ",
        "private ",
        "protected ",
        "export ",
        "import ",
        "from ",
        "package ",
        "module ",
        "const ",
        "static ",
        "var ",
        "let ",
        "#[",
    ];
    if DECL.iter().any(|d| t.starts_with(d)) {
        return true;
    }
    // top-level JSON/YAML key: `"key":` or `key:` near the line start
    let key_like = t.starts_with('"') && t.contains("\":");
    key_like || (t.contains(": ") && t.len() < 80 && !t.contains("  "))
}

/// True when the line carries a load-bearing anchor (file:line, error code, test
/// verdict, hash) — these are never dropped (reuses the codec's anchor predicate).
fn is_anchor(line: &str) -> bool {
    !anchor::extract_anchors(line).is_empty()
}

/// Extract `content` for a Read-class tool, stashing the original to rewind.
/// Returns None (pass-through) when the payload is small or extraction would not
/// save tokens.
pub fn extract_read(session: &str, content: &str) -> Option<Extracted> {
    if content.len() < MIN_BYTES {
        return None;
    }
    let lines: Vec<&str> = content.lines().collect();
    let n = lines.len();
    if n < 24 {
        return None;
    }
    let id = rewind::stash(session, content)?;
    let short_id: &str = &id;

    let mut out: Vec<String> = Vec::new();
    let mut dropped_lines = 0usize;
    let mut i = 0usize;
    while i < n {
        let keep = i < HEAD_LINES || is_structural(lines[i]) || is_anchor(lines[i]);
        if keep {
            out.push(lines[i].to_string());
            i += 1;
        } else {
            let start = i;
            while i < n && !(is_structural(lines[i]) || is_anchor(lines[i])) {
                i += 1;
            }
            let run = i - start; // dropped lines [start, i)
            dropped_lines += run;
            // 1-based inclusive retrieval range for the dropped run
            out.push(format!(
                "[glyphdown:extracted {run} lines {}-{} of {n} | retrieve id={short_id} range={}-{}]",
                start + 1,
                i,
                start + 1,
                i
            ));
        }
    }

    let extracted = out.join("\n");
    let original_tokens = codec::estimate_tokens(content);
    let extracted_tokens = codec::estimate_tokens(&extracted);
    // only engage if it actually saves (marker overhead can exceed tiny drops)
    if extracted_tokens >= original_tokens {
        return None;
    }
    Some(Extracted {
        text: extracted,
        rewind_id: id,
        original_tokens,
        extracted_tokens,
        dropped_lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iso(name: &str) -> std::sync::MutexGuard<'static, ()> {
        let g = crate::rewind::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("glyphdown-extract-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: test-only; the shared lock serializes all env-mutating tests.
        unsafe { std::env::set_var("GLYPHDOWN_REWIND_DIR", &dir) };
        g
    }

    fn big_read() -> String {
        // a realistic file-read: a few decls + lots of body, with anchors sprinkled.
        let mut s = String::new();
        s.push_str("# module docs\n");
        for f in 0..6 {
            s.push_str(&format!("def func_{f}(x):\n"));
            for b in 0..20 {
                if b == 7 {
                    s.push_str(&format!("    raise ValueError  # see src/mod.py:{f}{b}\n"));
                } else {
                    s.push_str(&format!(
                        "    body line {b} of {f} doing routine work here\n"
                    ));
                }
            }
        }
        s
    }

    #[test]
    fn small_payload_passes_through() {
        let _g = iso("small");
        assert!(extract_read("s", "short content").is_none());
    }

    #[test]
    fn extraction_saves_tokens_and_is_recoverable() {
        let _g = iso("save");
        let content = big_read();
        let r = extract_read("s", &content).expect("should extract a big read");
        assert!(
            r.original_tokens - r.extracted_tokens > 0,
            "must save tokens"
        );
        assert!(r.dropped_lines > 0);
        // the FULL original is recoverable byte-for-byte
        assert_eq!(
            rewind::retrieve("s", &r.rewind_id, None).as_deref(),
            Some(content.as_str())
        );
    }

    #[test]
    fn anchor_lines_are_never_dropped() {
        let _g = iso("anchor");
        let content = big_read();
        let r = extract_read("s", &content).unwrap();
        // every `src/mod.py:NN` anchor survives verbatim in the extracted text
        assert!(r.text.contains("src/mod.py:07"), "anchor line must be kept");
    }

    #[test]
    fn structural_declarations_kept() {
        let _g = iso("struct");
        let content = big_read();
        let r = extract_read("s", &content).unwrap();
        assert!(r.text.contains("def func_0(x):"), "decl outline kept");
        assert!(
            r.text.contains("[glyphdown:extracted"),
            "body runs become markers"
        );
    }

    #[test]
    fn marker_range_retrieves_the_dropped_lines() {
        let _g = iso("range");
        let content = big_read();
        let r = extract_read("s", &content).unwrap();
        // parse the first marker's range and confirm retrieve returns real body
        let marker = r
            .text
            .lines()
            .find(|l| l.contains("[glyphdown:extracted"))
            .unwrap();
        let range = marker.split("range=").nth(1).unwrap().trim_end_matches(']');
        let slice = rewind::retrieve("s", &r.rewind_id, Some(range)).unwrap();
        assert!(
            slice.contains("body line"),
            "retrieved range holds the dropped body"
        );
    }
}
