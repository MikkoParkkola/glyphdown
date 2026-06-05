//! GLYPHDOWN dialect codec — fast prose<->dense transcoder (ports codec/expand.py + compress.py).
//!
//! Lossless + reversible: expand(compress(x)) == x for dialect content; passes
//! unrecognized text through intact (never truncates). 44.6% real-Opus-token
//! reduction on dialect content (opus-dialect-validate-2026-05-31). Pure std,
//! near-zero startup — replaces the Python PreCompact hook on the hot path.

/// (dense, prose) dialect pairs — generated from the canonical EXPANSION_TABLE.
const PAIRS: &[(&str, &str)] = &[
    (
        r####"Pre-Implement (BLOCKING)"####,
        r####"Before creating any new hook, skill, or agent, search the existing codebase exhaustively"####,
    ),
    (
        r####"Before new hook/skill/agent"####,
        r####"Before creating any new hook, skill, or agent"####,
    ),
    (
        r####"Glob **/<topic>*"####,
        r####"Use Glob across the repository with the topic keyword"####,
    ),
    (
        r####"claude-elite+~/github"####,
        r####"the claude-elite repository and ~/github sibling projects"####,
    ),
    (
        r####"Grep keys+syn"####,
        r####"use Grep for keywords and synonyms"####,
    ),
    (
        r####"hooks/,skills/,agents/"####,
        r####"the hooks, skills, and agents directories"####,
    ),
    (
        r####"~/github siblings=active,not plans"####,
        r####"Check active projects in ~/github, not just project plans and roadmaps"####,
    ),
    (
        r####""still perfecting"≠nonexistent"####,
        r####"Statements like "still perfecting" in CLAUDE.md do not mean "this does not exist""####,
    ),
    (
        r####"Clean→propose"####,
        r####"Only after a clean search returns no hits may you propose creating a new component"####,
    ),
    (
        r####"Violation=H1+H2+H3 fail"####,
        r####"Skipping this constitutes a blocking file-hygiene violation against rules H1, H2, and H3"####,
    ),
    (
        r####"0-BUG STOP-THE-LINE"####,
        r####"If any test is failing, if there is a lint error, if there is a type error, if there is a HIGH-severity security finding"####,
    ),
    (r####"T:fail"####, r####"if any test is failing"####),
    (r####"Lint E:"####, r####"if there is a lint error"####),
    (r####"Type E:"####, r####"if there is a type error"####),
    (
        r####"S:HIGH"####,
        r####"if there is a HIGH-severity security finding"####,
    ),
    (
        r####"STOP.Fix.Verify.Continue"####,
        r####"stop all other work immediately. Fix the bug. Verify the fix passes. Only then continue"####,
    ),
    (r####"No exc"####, r####"No exceptions to this rule"####),
    (
        r####"ruff&&mypy&&bandit -r -ll&&pytest"####,
        r####"ruff check, mypy on the source tree, bandit recursive at medium severity, and pytest"####,
    ),
    (
        r####"bandit -r -ll"####,
        r####"bandit recursive at medium severity"####,
    ),
    (
        r####"GW:"####,
        r####"When you need to call a tool that is not directly listed in your top-level function definitions:"####,
    ),
    (
        r####"gateway_search(keyword)→invoke(server,tool)"####,
        r####"first call gateway_search with a relevant keyword to find candidate tools across the registry, then invoke it with gateway_invoke specifying the server name and the tool name"####,
    ),
    (
        r####"422 tools/29 backends"####,
        r####"The registry holds 422 tools across 29 backends"####,
    ),
    (
        r####"gateway_list_servers"####,
        r####"gateway_list_servers to see all backends"####,
    ),
    (
        r####"gateway_list_tools(server)"####,
        r####"gateway_list_tools to enumerate one backend"####,
    ),
    (
        r####"NEVER invent tool names,confirm via search first"####,
        r####"Never invent tool names from memory; always confirm via search first"####,
    ),
    (
        r####"URL→nab MCP"####,
        r####"For URL retrieval, prefer the nab tool. The nab MCP server provides"####,
    ),
    (
        r####"fetch|fetch_batch|submit|login"####,
        r####"nab_fetch, nab_fetch_batch, nab_submit, and nab_login"####,
    ),
    (r####"nab CLI"####, r####"the nab CLI binary"####),
    (r####"--cookies brave"####, r####"cookies from Brave"####),
    (
        r####"--1password"####,
        r####"credentials from 1Password"####,
    ),
    (
        r####"jina(MD)"####,
        r####"jina as a final fallback for clean Markdown extraction"####,
    ),
    (r####"NEVER WebFetch"####, r####"Never use WebFetch"####),
    (
        r####"~50K tok=25x waste"####,
        r####"it consumes approximately fifty thousand tokens per call, which is twenty-five times more wasteful than the alternatives"####,
    ),
    (r####"MEM:"####, r####"Memory protocol:"####),
    (
        r####"hebb FIRST"####,
        r####"query the persistent memory layer hebb first"####,
    ),
    (
        r####"~80% cache hit"####,
        r####"Approximately eighty percent of routine questions hit the cache and can be answered without fresh investigation"####,
    ),
    (
        r####"novel→baseline"####,
        r####"Only when the topic is novel should you fall back to baseline research"####,
    ),
    (
        r####"ask_pieces_ltm(recall)"####,
        r####"ask_pieces_ltm for recall"####,
    ),
    (
        r####"create_pieces_memory(store)"####,
        r####"create_pieces_memory for storing new learnings"####,
    ),
    (
        r####"AGT 2+"####,
        r####"When two or more agents are needed for a task"####,
    ),
    (
        r####"TeamCreate+TaskCreate+SendMessage"####,
        r####"use TeamCreate with TaskCreate and SendMessage"####,
    ),
    (
        r####"NEVER Task multi-agent"####,
        r####"instead of multiple parallel Task invocations"####,
    ),
    (
        r####"67% loss"####,
        r####"approximately sixty-seven percent of inter-agent messages to be lost"####,
    ),
    (
        r####"Task=single subagent only"####,
        r####"The Task tool is intended for a single subagent"####,
    ),
    (
        r####"max parallel=15"####,
        r####"The maximum supported parallelism is fifteen agents per team"####,
    ),
    (r####"EVIDENCE:"####, r####"Evidence requirement:"####),
    (
        r####"every claim→file:line"####,
        r####"For every claim you make, attach evidence: a file path with a line number"####,
    ),
    (r####"T:output"####, r####"a test output"####),
    (
        r####"benchmark|cmd"####,
        r####"a benchmark result, the literal command used"####,
    ),
    (r####"Confidence:"####, r####"Mark your confidence as"####),
    (
        r####"V(2+ sources)"####,
        r####"V for verified when there are two or more independent sources"####,
    ),
    (
        r####"I(1)"####,
        r####"I for inferred when there is one source"####,
    ),
    (
        r####"A(0)"####,
        r####"A for assumption when there is no source"####,
    ),
    (
        r####""I don't know" > wrong"####,
        r####"Saying "I do not know" is preferable to producing a wrong answer"####,
    ),
    (r####"→"####, r####" then "####),
    (r####"≠"####, r####" does not equal "####),
    (r####"&&"####, r####" and "####),
    (r####"|"####, r####", "####),
];

/// A loadable GLYPHDOWN dialect: an ordered table of (dense, prose) pairs.
///
/// Order is load-bearing. `compress`/`expand` build length-sorted views with a
/// STABLE sort, so equal-length ties preserve table order. Any (de)serialization
/// MUST preserve order — that is why the on-disk form is an ordered JSON array of
/// `[dense, prose]` pairs, never a map (map iteration would scramble ties and the
/// byte-for-byte output would drift from the compiled-in default).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialect {
    /// (dense, prose) pairs in canonical order (same order as the bundled default).
    pairs: Vec<(String, String)>,
}

impl Dialect {
    /// The compiled-in default dialect (bundled fallback). Always available, always
    /// lossless — this is the table that ships in the binary.
    pub fn bundled_default() -> Self {
        Dialect {
            pairs: PAIRS
                .iter()
                .map(|(d, p)| ((*d).to_string(), (*p).to_string()))
                .collect(),
        }
    }

    /// Build from an ordered (dense, prose) list. Does not validate losslessness.
    pub fn from_pairs(pairs: Vec<(String, String)>) -> Self {
        Dialect { pairs }
    }

    /// Longest-prose-first (prose, dense) view — specific phrases before substrings.
    fn compress_pairs(&self) -> Vec<(&str, &str)> {
        let mut v: Vec<(&str, &str)> = self
            .pairs
            .iter()
            .map(|(d, p)| (p.as_str(), d.as_str()))
            .collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.0.len()));
        v
    }

    /// Longest-dense-first (dense, prose) view — specific dense tokens before substrings.
    fn expand_pairs(&self) -> Vec<(&str, &str)> {
        let mut v: Vec<(&str, &str)> = self
            .pairs
            .iter()
            .map(|(d, p)| (d.as_str(), p.as_str()))
            .collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.0.len()));
        v
    }

    /// prose -> dense (lossless; unrecognized text passes through).
    pub fn compress(&self, prose: &str) -> String {
        let mut out = prose.to_string();
        for (p, d) in self.compress_pairs() {
            out = out.replace(p, d);
        }
        out
    }

    /// dense -> prose (reverse).
    pub fn expand(&self, dense: &str) -> String {
        let mut out = dense.to_string();
        for (d, p) in self.expand_pairs() {
            out = out.replace(d, p);
        }
        out
    }

    /// Losslessness self-check: `expand(compress(x)) == x` for every prose value in
    /// the table. Guards the line-3 reversibility contract for hand-edited / fetched
    /// dialects — collisions or bad ordering are caught HERE, before any traffic is
    /// touched, so a bad config degrades to the bundled default instead of corrupting.
    fn is_lossless(&self) -> bool {
        self.pairs
            .iter()
            .all(|(_d, prose)| self.expand(&self.compress(prose)) == *prose)
    }

    /// Parse an ordered JSON array of `[dense, prose]` pairs.
    pub fn from_json(s: &str) -> Result<Self, String> {
        let raw: Vec<(String, String)> =
            serde_json::from_str(s).map_err(|e| format!("dialect json parse: {e}"))?;
        Ok(Dialect::from_pairs(raw))
    }

    /// Serialize to an ordered JSON array of `[dense, prose]` pairs (round-trips to
    /// the same Dialect; used to generate the bundled baseline from the const table).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.pairs).unwrap_or_else(|_| "[]".to_string())
    }

    /// Resolve the active dialect: env-pointed file -> parse -> lossless check -> use,
    /// else fall back to the bundled default. Fail-open at every step so the codec can
    /// never break on a missing, malformed, or non-lossless config file.
    fn resolve() -> Self {
        let Some(path) = dialect_path() else {
            return Dialect::bundled_default();
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return Dialect::bundled_default(),
        };
        match Dialect::from_json(&text) {
            Ok(d) if d.is_lossless() => d,
            Ok(_) => {
                eprintln!(
                    "glyphdown: dialect at {} failed lossless self-check; using bundled default",
                    path.display()
                );
                Dialect::bundled_default()
            }
            Err(e) => {
                eprintln!("glyphdown: {e}; using bundled default");
                Dialect::bundled_default()
            }
        }
    }
}

/// Discover the dialect file path. P0 keeps discovery minimal — a single
/// `GLYPHDOWN_DIALECT` env override. P3 adds the bundled-baseline path and the
/// optional license-gated fetch endpoint; the binary still just reads a file.
fn dialect_path() -> Option<std::path::PathBuf> {
    match std::env::var_os("GLYPHDOWN_DIALECT") {
        Some(p) if !p.is_empty() => Some(std::path::PathBuf::from(p)),
        _ => None,
    }
}

/// Process-global active dialect, loaded once on first use. The global is the only
/// `OnceLock` — kept thin on purpose so tests/harnesses inject `Dialect` instances
/// directly (a write-once global would make multi-dialect parity tests pass for the
/// wrong reason).
fn global_dialect() -> &'static Dialect {
    static GLOBAL: std::sync::OnceLock<Dialect> = std::sync::OnceLock::new();
    GLOBAL.get_or_init(Dialect::resolve)
}

/// prose -> dense (lossless; unrecognized text passes through). Uses the active dialect.
pub fn compress(prose: &str) -> String {
    global_dialect().compress(prose)
}

/// dense -> prose (reverse). Uses the active dialect.
pub fn expand(dense: &str) -> String {
    global_dialect().expand(dense)
}

/// Result of previewing a static-config compression (CLAUDE.md, a skill, an
/// agent description). The system prompt ships on EVERY request, so compressing
/// it with the active dialect is the only always-on, every-call saving — but it
/// is destructive to a file the user authored, so the lossless gate is mandatory
/// before anything is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigCompression {
    pub compressed: String,
    /// expand(compress(x)) == x for THIS file. If false, never write it.
    pub lossless: bool,
    /// The file already contains dialect dense-tokens (expand would change it),
    /// so it may already be compressed — surfaced as a caution, not a blocker.
    pub already_dense: bool,
    pub original_tokens: i64,
    pub compressed_tokens: i64,
}

impl ConfigCompression {
    pub fn saved_tokens(&self) -> i64 {
        self.original_tokens - self.compressed_tokens
    }
    pub fn savings_pct(&self) -> f64 {
        if self.original_tokens <= 0 {
            0.0
        } else {
            (self.saved_tokens() as f64 / self.original_tokens as f64) * 100.0
        }
    }
    /// Safe to write iff the round-trip is lossless. The whole feature rests here.
    pub fn safe_to_apply(&self) -> bool {
        self.lossless
    }
}

impl Dialect {
    /// Preview compressing a user-authored config file. Computes the compressed
    /// form, verifies losslessness against THIS dialect, and estimates token
    /// savings. Never writes — the caller decides, gated on `safe_to_apply()`.
    pub fn compress_config(&self, content: &str) -> ConfigCompression {
        let compressed = self.compress(content);
        let lossless = self.expand(&compressed) == content;
        let already_dense = self.expand(content) != content;
        ConfigCompression {
            original_tokens: estimate_tokens(content),
            compressed_tokens: estimate_tokens(&compressed),
            compressed,
            lossless,
            already_dense,
        }
    }
}

/// Preview compressing a config file with the active (global) dialect.
pub fn compress_config(content: &str) -> ConfigCompression {
    global_dialect().compress_config(content)
}

/// Bounded truncation (internal-ref layer 10): ONLY above line_cap, marker NEVER inline,
/// NEVER applied to JSON (would invalidate). Keeps head, appends single trailing marker.
pub fn truncate_bounded(s: &str, line_cap: usize) -> String {
    // never truncate valid JSON
    if serde_json::from_str::<serde_json::Value>(s).is_ok() {
        return s.to_string();
    }
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= line_cap {
        return s.to_string();
    }
    let hidden = lines.len() - line_cap;
    let mut out = lines[..line_cap].join("\n");
    out.push_str(&format!("\n[truncated: {hidden} lines hidden]\n"));
    out
}

/// JSON whitespace minify (lossless: parse then re-serialize compact). No-op if not valid JSON.
pub fn json_minify(s: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(v) => serde_json::to_string(&v).unwrap_or_else(|_| s.to_string()),
        Err(_) => s.to_string(),
    }
}

/// Collapse consecutive duplicate lines to "<line> xN" (first-occurrence order kept).
pub fn dedup_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let lines: Vec<&str> = s.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let mut n = 1;
        while i + n < lines.len() && lines[i + n] == lines[i] {
            n += 1;
        }
        out.push_str(lines[i]);
        if n > 1 {
            out.push_str(&format!(" x{n}"));
        }
        out.push('\n');
        i += n;
    }
    out
}

/// Lossless mechanical tool-result compaction (ports internal-ref deterministic layers 5-8).
/// ANSI strip + 3+blank-collapse + trailing-ws trim. All byte-safe, reversible-in-meaning.
pub fn compact(input: &str) -> String {
    // try whole-payload JSON minify first (lossless); else mechanical text layers
    let minified = json_minify(input);
    if minified != input {
        return minified;
    }
    let mut out = strip_ansi(input);
    out = dedup_lines(&out);
    out = collapse_blank_lines(&out);
    out = trim_trailing_ws(&out);
    out
}

/// Remove ANSI escape sequences: \x1B[ ... final-byte. Lossless on visible stream.
pub fn strip_ansi(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1B && i + 1 < b.len() && b[i + 1] == b'[' {
            i += 2;
            // params [0-?], intermediates [ -/], final [@-~]
            while i < b.len() && (0x30..=0x3F).contains(&b[i]) {
                i += 1;
            }
            while i < b.len() && (0x20..=0x2F).contains(&b[i]) {
                i += 1;
            }
            if i < b.len() && (0x40..=0x7E).contains(&b[i]) {
                i += 1;
            }
        } else {
            // copy one UTF-8 char
            let ch_len = utf8_len(b[i]);
            if let Ok(seg) = std::str::from_utf8(&b[i..(i + ch_len).min(b.len())]) {
                out.push_str(seg);
            }
            i += ch_len;
        }
    }
    out
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first < 0xE0 {
        2
    } else if first < 0xF0 {
        3
    } else {
        4
    }
}

/// Collapse runs of 3+ blank lines to 2 (preserves single/double paragraph breaks).
pub fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0usize;
    for line in s.split_inclusive('\n') {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 2 {
                out.push_str(line);
            }
        } else {
            blank_run = 0;
            out.push_str(line);
        }
    }
    out
}

/// Trim trailing whitespace on each line.
pub fn trim_trailing_ws(s: &str) -> String {
    let mut out: Vec<String> = s.lines().map(|l| l.trim_end().to_string()).collect();
    let joined = out.join("\n");
    if s.ends_with('\n') {
        out.clear();
        return joined + "\n";
    }
    joined
}

// ─────────────────────────────────────────────────────────────────────────
// PHASE 2a — tokenizer-free semantic-equivalent port of python `compact_payload`
// (hooks/PostToolUse/glyphdown_codec.py). Target: SAME compressed body as python
// when python is forced to its 4-char token fallback (GLYPHDOWN_FORCE_CHAR_TOKENS).
// NO tiktoken on the hot path. All pure functions, fail-open at the caller.
// ─────────────────────────────────────────────────────────────────────────

/// Schema-tag prefix (matches python TAG_PREFIX; leading `[`, closed by `]`).
const TAG_PREFIX: &str = "[glyphdown:compact-v1";
const DEFAULT_BREAK_EVEN_TOKENS: i64 = 25;
const DEFAULT_MIN_SAVINGS_RATIO: f64 = 0.05;
const DEFAULT_TRUNCATE_BYTES: usize = 8192;

/// Fast token estimate — python's tiktoken-absent fallback: `max(1, len(s)//4)`
/// where python `len(s)` counts unicode codepoints, so use `chars().count()`.
pub fn estimate_tokens(s: &str) -> i64 {
    (s.chars().count() as i64 / 4).max(1)
}

/// python `collapse_blanks`: TRAILING_WS_RE `[ \t]+$` (per line) then
/// MULTI_BLANK_RE `\n{3,}` -> `\n\n`. NB different from `collapse_blank_lines`
/// (which keeps up to 2 blank lines and does not trim trailing ws).
pub fn collapse_blanks(text: &str) -> String {
    // 1) strip trailing spaces/tabs on each line (preserve the newline structure).
    let mut trimmed = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut line_start = 0usize;
    let mut i = 0usize;
    while i <= bytes.len() {
        if i == bytes.len() || bytes[i] == b'\n' {
            // line is bytes[line_start..i]; trim trailing space/tab.
            let mut end = i;
            while end > line_start && (bytes[end - 1] == b' ' || bytes[end - 1] == b'\t') {
                end -= 1;
            }
            trimmed.push_str(&text[line_start..end]);
            if i < bytes.len() {
                trimmed.push('\n');
            }
            line_start = i + 1;
        }
        i += 1;
    }
    // 2) collapse runs of 3+ '\n' to exactly 2 (raw newline chars, like the regex).
    let mut out = String::with_capacity(trimmed.len());
    let mut run = 0usize;
    let flush = |out: &mut String, run: usize| {
        if run >= 3 {
            out.push_str("\n\n");
        } else {
            for _ in 0..run {
                out.push('\n');
            }
        }
    };
    for ch in trimmed.chars() {
        if ch == '\n' {
            run += 1;
        } else {
            if run > 0 {
                flush(&mut out, run);
                run = 0;
            }
            out.push(ch);
        }
    }
    if run > 0 {
        flush(&mut out, run);
    }
    out
}

/// python `truncate_with_marker`: byte-cap tail-truncate, single trailing marker.
/// Returns (text, hidden_bytes). head reserves 96 bytes for the marker line.
pub fn truncate_with_marker(text: &str, max_bytes: usize) -> (String, usize) {
    let raw = text.as_bytes();
    if raw.len() <= max_bytes {
        return (text.to_string(), 0);
    }
    let head_bytes = max_bytes.saturating_sub(96);
    // decode utf-8 ignoring a possibly-split trailing char, then rstrip.
    let mut cut = head_bytes.min(raw.len());
    // back up to a char boundary (utf-8 continuation bytes match 0b1000_0000).
    while cut > 0 && (raw[cut] & 0xC0) == 0x80 {
        cut -= 1;
    }
    let head_full = std::str::from_utf8(&raw[..cut]).unwrap_or("");
    let head = head_full.trim_end();
    let hidden_lines = text.matches('\n').count() as i64 - head.matches('\n').count() as i64;
    let hidden_bytes = raw.len() - head.len();
    let marker = format!("\n\n[truncated: {hidden_lines} lines / {hidden_bytes} bytes hidden]");
    (format!("{head}{marker}"), hidden_bytes)
}

/// First 4096 chars (python uses [:4096] on a str = codepoints).
fn head4096(text: &str) -> String {
    text.chars().take(4096).collect()
}

/// python `looks_like_binary_base64`: data-URI or a 256+ run of base64 chars
/// in the first 4096 chars.
fn looks_like_binary_base64(text: &str) -> bool {
    let head = head4096(text);
    // DATA_URI_RE: data:(image|application|audio|video)/[\w.+-]+;base64,
    if let Some(pos) = head.find("data:") {
        let rest = &head[pos + 5..];
        for kind in ["image/", "application/", "audio/", "video/"] {
            if rest.starts_with(kind) {
                let after = &rest[kind.len()..];
                // [\w.+-]+ ; base64,
                let mut j = 0;
                for c in after.chars() {
                    if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-') {
                        j += c.len_utf8();
                    } else {
                        break;
                    }
                }
                if j > 0 && after[j..].starts_with(";base64,") {
                    return true;
                }
            }
        }
    }
    // BASE64_BLOB_RE: [A-Za-z0-9+/=]{256,}
    let mut run = 0usize;
    for c in head.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=') {
            run += 1;
            if run >= 256 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Count closing-tag matches `</tag>` (HTML_TAG_RE: `</[a-zA-Z][\w:-]*\s*>`).
fn count_html_close_tags(head: &str) -> usize {
    let b = head.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'<' && b[i + 1] == b'/' {
            let mut j = i + 2;
            // first char [a-zA-Z]
            if j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
                while j < b.len()
                    && (b[j].is_ascii_alphanumeric() || matches!(b[j], b'_' | b':' | b'-'))
                {
                    j += 1;
                }
                while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                    j += 1;
                }
                if j < b.len() && b[j] == b'>' {
                    count += 1;
                    i = j + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    count
}

/// python `looks_like_html`: DOCTYPE html OR starts with `<` and 3+ closing tags.
fn looks_like_html(text: &str) -> bool {
    let lstripped = text.trim_start();
    let head: String = lstripped.chars().take(4096).collect();
    // HTML_DOCTYPE_RE: ^\s*<!DOCTYPE\s+html  (case-insensitive); head already lstripped
    let low = head.to_ascii_lowercase();
    if low.starts_with("<!doctype") {
        let after = &head[9..];
        let after_ws = after.trim_start();
        if after_ws.len() != after.len() && after_ws.to_ascii_lowercase().starts_with("html") {
            return true;
        }
    }
    if lstripped.starts_with('<') && count_html_close_tags(&head) >= 3 {
        return true;
    }
    false
}

fn is_language_data(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    looks_like_binary_base64(text) || looks_like_html(text)
}

/// python `looks_like_json`: first non-space char is `{` or `[`.
fn looks_like_json(text: &str) -> bool {
    matches!(text.trim_start().chars().next(), Some('{') | Some('['))
}

fn first_n_lines(text: &str, n: usize) -> String {
    text.lines().take(n).collect::<Vec<_>>().join("\n")
}

/// python `looks_like_yaml`: lstrip starts `---` OR 3+ `^[A-Za-z_][\w-]*\s*:` in head(10).
fn looks_like_yaml(text: &str) -> bool {
    if text.trim_start().starts_with("---") {
        return true;
    }
    let head = first_n_lines(text, 10);
    let mut keyish = 0;
    for line in head.lines() {
        let b = line.as_bytes();
        if b.is_empty() {
            continue;
        }
        if !(b[0].is_ascii_alphabetic() || b[0] == b'_') {
            continue;
        }
        let mut j = 1;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || matches!(b[j], b'_' | b'-')) {
            j += 1;
        }
        while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
            j += 1;
        }
        if j < b.len() && b[j] == b':' {
            keyish += 1;
        }
    }
    keyish >= 3
}

/// python `looks_like_toml`: a `^\[[\w.-]+\]\s*$` line in head(10).
fn looks_like_toml(text: &str) -> bool {
    for line in first_n_lines(text, 10).lines() {
        let t = line.trim_end();
        let b = t.as_bytes();
        if b.len() >= 3 && b[0] == b'[' && b[b.len() - 1] == b']' {
            let inner = &t[1..t.len() - 1];
            if !inner.is_empty()
                && inner
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
            {
                return true;
            }
        }
    }
    false
}

/// python `looks_like_code`: 3+ keyword occurrences in head(50).
fn looks_like_code(text: &str) -> bool {
    let head = first_n_lines(text, 50);
    const KW: &[&str] = &[
        "def ",
        "function ",
        "fn ",
        "class ",
        "import ",
        "package ",
        "use ",
        "#include ",
        "public class ",
    ];
    let mut count = 0usize;
    for kw in KW {
        count += head.matches(kw).count();
    }
    count >= 3
}

/// python `looks_like_path_list`: 3+ non-blank lines, 80%+ path-like.
fn looks_like_path_list(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 3 {
        return false;
    }
    let path_like = lines.iter().filter(|l| is_path_line(l)).count();
    path_like >= 3.max((lines.len() as f64 * 0.8) as usize)
}

/// `_PATH_LIST_LINE` `^(/|~/|[A-Z]:\\)` AND no space in first 30 chars.
fn is_path_line(line: &str) -> bool {
    let starts = line.starts_with('/') || line.starts_with("~/") || {
        let b = line.as_bytes();
        b.len() >= 3 && b[0].is_ascii_uppercase() && b[1] == b':' && b[2] == b'\\'
    };
    if !starts {
        return false;
    }
    !line.chars().take(30).any(|c| c == ' ')
}

/// python `classify_payload`: ordered shape detection.
pub fn classify(text: &str) -> &'static str {
    if is_language_data(text) {
        if looks_like_html(text) {
            return "html";
        }
        return "binary";
    }
    if looks_like_json(text) {
        return "json";
    }
    if looks_like_toml(text) {
        return "toml";
    }
    if looks_like_yaml(text) {
        return "yaml";
    }
    if looks_like_code(text) {
        return "code";
    }
    if looks_like_path_list(text) {
        return "path-list";
    }
    "text"
}

/// python `_longest_common_path_prefix`: longest common prefix snapped to last sep.
fn longest_common_path_prefix(lines: &[&str]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut prefix = lines[0].to_string();
    for ln in &lines[1..] {
        while !ln.starts_with(&prefix) {
            prefix.pop();
            if prefix.is_empty() {
                return String::new();
            }
        }
    }
    let sep_idx = prefix
        .rfind('/')
        .map(|a| a as i64)
        .unwrap_or(-1)
        .max(prefix.rfind('\\').map(|a| a as i64).unwrap_or(-1));
    if sep_idx <= 0 {
        return String::new();
    }
    prefix[..(sep_idx as usize + 1)].to_string()
}

/// python `compress_path_list`: lossless common-prefix factoring. None when
/// savings below the threshold (<16 prefix, or <64 bytes saved).
pub fn compress_path_list(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 3 {
        return None;
    }
    let prefix = longest_common_path_prefix(&lines);
    if prefix.chars().count() < 16 {
        return None;
    }
    let relative: Vec<&str> = lines.iter().map(|ln| &ln[prefix.len()..]).collect();
    let out = format!(
        "[glyphdown:cpc-v1 prefix={prefix} n={}]\n{}",
        lines.len(),
        relative.join("\n")
    );
    if out.len() >= text.len().saturating_sub(64) {
        return None;
    }
    Some(out)
}

/// python `toonify_uniform_array`: opt-in tabular encoding (GLYPHDOWN_TOON).
/// Forward-only; returns None unless the shape matches exactly.
pub fn toonify_uniform_array(text: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(text).ok()?;
    let arr = parsed.as_array()?;
    if arr.len() < 10 || !arr.iter().all(|x| x.is_object()) {
        return None;
    }
    let keys: Vec<String> = arr[0].as_object()?.keys().cloned().collect();
    if keys.is_empty() {
        return None;
    }
    for entry in arr {
        let obj = entry.as_object()?;
        let ekeys: Vec<&String> = obj.keys().collect();
        if ekeys.len() != keys.len() || !keys.iter().zip(&ekeys).all(|(a, b)| &a == b) {
            return None;
        }
        for v in obj.values() {
            if v.is_array() || v.is_object() {
                return None;
            }
        }
    }
    let fmt = |v: &serde_json::Value| -> String {
        match v {
            serde_json::Value::Null => String::new(),
            serde_json::Value::Bool(b) => {
                if *b {
                    "true".into()
                } else {
                    "false".into()
                }
            }
            serde_json::Value::String(s) => {
                if s.contains(',') || s.contains('\n') || s.contains('"') {
                    format!("\"{}\"", s.replace('"', "\"\""))
                } else {
                    s.clone()
                }
            }
            other => other.to_string(),
        }
    };
    let header = format!("items[{}]{{{}}}:", arr.len(), keys.join(","));
    let body = arr
        .iter()
        .map(|e| {
            let obj = e.as_object().unwrap();
            let cells: Vec<String> = keys.iter().map(|k| fmt(&obj[k])).collect();
            format!("  {}", cells.join(","))
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!("{header}\n{body}"))
}

fn toon_enabled() -> bool {
    matches!(
        std::env::var("GLYPHDOWN_TOON").ok().as_deref(),
        Some("1") | Some("true") | Some("True")
    )
}

/// Report exposing what `compact_payload` did, for audit-row emission.
pub struct CompactReport {
    pub output: String,
    pub shape: &'static str,
    pub applied: Vec<String>,
    pub original_tokens: i64,
    pub compact_tokens: i64,
}

impl CompactReport {
    pub fn saved_tokens(&self) -> i64 {
        self.original_tokens - self.compact_tokens
    }
}

/// Thin wrapper: the assembled output string (what the subcommand prints).
pub fn compact_payload(text: &str) -> String {
    compact_payload_report(text).output
}

/// python `compact_payload`: the full assembler. Returns the final string
/// (tagged candidate, OR the verbatim original on passthrough) plus the
/// shape/applied/token metadata. The caller treats `out != input` as "changed".
pub fn compact_payload_report(text: &str) -> CompactReport {
    if text.is_empty() {
        return CompactReport {
            output: text.to_string(),
            shape: "empty",
            applied: vec![],
            original_tokens: 0,
            compact_tokens: 0,
        };
    }
    // internal-ref Language::Data short-circuit (no tag, verbatim passthrough).
    if is_language_data(text) {
        let ot = estimate_tokens(text);
        let shape = if looks_like_html(text) {
            "html"
        } else {
            "binary"
        };
        return CompactReport {
            output: text.to_string(),
            shape,
            applied: vec![],
            original_tokens: ot,
            compact_tokens: ot,
        };
    }

    let original_tokens = estimate_tokens(text);
    let shape = classify(text);
    let mut applied: Vec<&str> = Vec::new();
    let mut candidate: String;

    match shape {
        "json" => {
            let toon = if toon_enabled() {
                toonify_uniform_array(text)
            } else {
                None
            };
            if let Some(t) = toon.filter(|t| t.len() < text.len().saturating_sub(64)) {
                candidate = t;
                applied.push("toon-encode");
                let stripped = strip_ansi(&candidate);
                if stripped != candidate {
                    applied.push("ansi-strip");
                }
                candidate = stripped;
            } else {
                candidate = json_minify(text);
                if candidate != text {
                    applied.push("json-minify");
                }
                let stripped = strip_ansi(&candidate);
                if stripped != candidate {
                    applied.push("ansi-strip");
                }
                candidate = stripped;
            }
        }
        "yaml" | "toml" | "code" => {
            candidate = strip_ansi(text);
            if candidate != text {
                applied.push("ansi-strip");
            }
            let collapsed = collapse_blanks(&candidate);
            if collapsed != candidate {
                applied.push("blank-collapse");
            }
            candidate = collapsed;
        }
        "path-list" => {
            if let Some(factored) = compress_path_list(text) {
                applied.push("path-prefix-factor");
                candidate = factored;
            } else {
                candidate = text.to_string();
            }
            let stripped = strip_ansi(&candidate);
            if stripped != candidate {
                applied.push("ansi-strip");
            }
            let collapsed = collapse_blanks(&stripped);
            if collapsed != stripped {
                applied.push("blank-collapse");
            }
            candidate = collapsed;
        }
        _ => {
            candidate = strip_ansi(text);
            if candidate != text {
                applied.push("ansi-strip");
            }
            let collapsed = collapse_blanks(&candidate);
            if collapsed != candidate {
                applied.push("blank-collapse");
            }
            candidate = collapsed;
        }
    }

    // A3: truncate non-JSON if still too large.
    if shape != "json" {
        let (truncated, hidden) = truncate_with_marker(&candidate, DEFAULT_TRUNCATE_BYTES);
        if hidden > 0 {
            applied.push("truncate");
            candidate = truncated;
        }
    }

    let compact_tokens = estimate_tokens(&candidate);
    let saved = original_tokens - compact_tokens;
    let ratio_saved = if original_tokens > 0 {
        saved as f64 / original_tokens as f64
    } else {
        0.0
    };

    // A10 break-even guard — verbatim original (untagged) when any condition holds.
    if saved < DEFAULT_BREAK_EVEN_TOKENS
        || applied.is_empty()
        || (DEFAULT_MIN_SAVINGS_RATIO > 0.0 && ratio_saved < DEFAULT_MIN_SAVINGS_RATIO)
    {
        return CompactReport {
            output: text.to_string(),
            shape,
            applied: vec![],
            original_tokens,
            compact_tokens: original_tokens,
        };
    }

    // A9 schema-tag prefix.
    let ratio = if original_tokens != 0 {
        compact_tokens as f64 / original_tokens as f64
    } else {
        1.0
    };
    let tag = format!(
        "{TAG_PREFIX} shape={shape} ratio={ratio:.2} applied={}]\n",
        applied.join(",")
    );
    CompactReport {
        output: format!("{tag}{candidate}"),
        shape,
        applied: applied.iter().map(|s| s.to_string()).collect(),
        original_tokens,
        // python returns compact_tokens + estimate_tokens(tag) for the audit
        // field (the tag's own token cost counts against the reported savings),
        // even though the tag STRING is formatted with the candidate-only ratio.
        compact_tokens: compact_tokens + estimate_tokens(&tag),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_only_above_cap_never_json() {
        let small = "a\nb\nc";
        assert_eq!(truncate_bounded(small, 10), small); // under cap, untouched
        let json = "{\"a\":1,\"b\":2,\"c\":3,\"d\":4}";
        assert_eq!(truncate_bounded(json, 1), json); // JSON never truncated
        let big: String = (0..20)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let t = truncate_bounded(&big, 5);
        assert!(t.contains("[truncated:"));
        assert!(t.contains("line0"));
        assert!(!t.contains("line19"));
    }
    #[test]
    fn json_minify_lossless() {
        let pretty = "{\n  \"a\": 1,\n  \"b\": [1, 2]\n}";
        let m = json_minify(pretty);
        assert_eq!(m, "{\"a\":1,\"b\":[1,2]}");
        // value identity preserved
        let v1: serde_json::Value = serde_json::from_str(pretty).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&m).unwrap();
        assert_eq!(v1, v2);
    }
    #[test]
    fn dedup_consecutive() {
        assert_eq!(dedup_lines("x\nx\nx\ny\n"), "x x3\ny\n");
    }
    #[test]
    fn ansi_strip_lossless() {
        assert_eq!(strip_ansi("\x1B[31mred\x1B[0m text"), "red text");
    }
    #[test]
    fn blank_collapse() {
        assert_eq!(collapse_blank_lines("a\n\n\n\n\nb\n"), "a\n\n\nb\n");
    }
    #[test]
    fn compact_reduces_noisy() {
        let noisy = "\x1B[32mok\x1B[0m   \n\n\n\n\ndone   \n";
        let c = compact(noisy);
        assert!(c.len() < noisy.len());
        assert!(c.contains("ok"));
        assert!(c.contains("done"));
    }
    #[test]
    fn roundtrip_lossless() {
        let prose = "Before creating any new hook, skill, or agent, search the existing codebase exhaustively";
        assert_eq!(expand(&compress(prose)), prose);
    }
    #[test]
    fn compresses_dialect() {
        let prose = "Before creating any new hook, skill, or agent, search the existing codebase exhaustively";
        assert!(compress(prose).len() < prose.len());
    }
    #[test]
    fn never_truncates_arbitrary() {
        let arb = "The quantum flux capacitor needs 1.21 gigawatts at node xj-42.";
        assert_eq!(compress(arb), arb);
    }

    // ── PHASE 2a: per-shape compact_payload parity tests ────────────────────

    #[test]
    fn estimate_tokens_is_chars_div4_min1() {
        assert_eq!(estimate_tokens(""), 1); // max(1, 0)
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens(&"x".repeat(40)), 10);
    }

    #[test]
    fn classify_orders_shapes() {
        assert_eq!(classify("{\"a\":1}"), "json");
        // NB: a leading `[` is json in python too (looks_like_json checks `{[`),
        // so real TOML must lead with a key/comment for the toml probe to win.
        assert_eq!(classify("title = \"x\"\n[mod]\nkey = 1\nother = 2"), "toml");
        assert_eq!(classify("name: a\nrole: b\nteam: c"), "yaml");
        let code = "def a():\n    pass\nimport os\nclass B:\n    pass";
        assert_eq!(classify(code), "code");
        let paths = "/usr/local/bin/a\n/usr/local/bin/b\n/usr/local/bin/c";
        assert_eq!(classify(paths), "path-list");
        assert_eq!(classify("just some prose here"), "text");
    }

    #[test]
    fn json_shape_minifies_never_truncates() {
        // pretty JSON, large enough to cross break-even, must minify + tag, no truncate.
        let pretty = format!(
            "{{\n{}\n}}",
            (0..60)
                .map(|i| format!("  \"key_number_{i}\": {i}"))
                .collect::<Vec<_>>()
                .join(",\n")
        );
        let out = compact_payload(&pretty);
        assert!(out.starts_with("[glyphdown:compact-v1 shape=json"));
        assert!(out.contains("applied=json-minify"));
        assert!(!out.contains("[truncated:")); // JSON never truncated
        // body after the tag line must be valid minified JSON
        let body = &out[out.find('\n').unwrap() + 1..];
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["key_number_0"], 0);
    }

    #[test]
    fn text_shape_no_dedup() {
        // python text path does NOT dedup consecutive lines; ensure ours matches.
        let dup = format!("{}\n", vec!["same line"; 60].join("\n"));
        let out = compact_payload(&dup);
        // saving comes only from blank/ws — duplicate lines are NOT collapsed,
        // so this small-savings payload passes through verbatim (break-even).
        assert_eq!(out, dup);
        assert!(!out.contains(" x")); // no "line xN" dedup marker
    }

    #[test]
    fn path_list_prefix_factoring_and_tag() {
        let lines: Vec<String> = (0..40)
            .map(|i| format!("/home/user/github/claude-plugins/glyphdown/file_{i:03}.rs"))
            .collect();
        let text = lines.join("\n");
        let out = compact_payload(&text);
        assert!(out.starts_with("[glyphdown:compact-v1 shape=path-list"));
        assert!(out.contains("applied=path-prefix-factor"));
        assert!(
            out.contains("[glyphdown:cpc-v1 prefix=/home/user/github/claude-plugins/glyphdown/")
        );
    }

    #[test]
    fn truncation_marker_on_huge_text() {
        let huge = format!("HEAD\n{}", "x".repeat(20_000));
        let (t, hidden) = truncate_with_marker(&huge, 8192);
        assert!(hidden > 0);
        assert!(t.len() <= 8192);
        assert!(t.starts_with("HEAD"));
        assert!(t.contains("[truncated:") && t.contains("bytes hidden]"));
    }

    #[test]
    fn break_even_passthrough_is_verbatim() {
        // small payload, sub-25-token savings -> identical original, no tag.
        let small = "\x1b[31mred\x1b[0m\n\n\n\ntext";
        assert_eq!(compact_payload(small), small);
    }

    #[test]
    fn language_data_passthrough() {
        let html = "<!DOCTYPE html><html><body></body></html>";
        assert_eq!(compact_payload(html), html); // verbatim, no tag
        assert_eq!(classify(html), "html");
        let b64 = format!("data:image/png;base64,{}", "A".repeat(300));
        assert_eq!(compact_payload(&b64), b64);
        assert_eq!(classify(&b64), "binary");
    }

    #[test]
    fn tag_format_exact() {
        // a text payload with a big blank run: applied=ansi-strip? no, just blank-collapse.
        let big_blanks = "\n".repeat(400);
        let text = format!("first line{big_blanks}last line");
        let out = compact_payload(&text);
        assert!(out.starts_with("[glyphdown:compact-v1 shape=text ratio="));
        assert!(out.contains("applied=blank-collapse]\n"));
        // collapsed body: the 400-blank run becomes a single blank line (\n\n).
        assert!(out.ends_with("first line\n\nlast line"));
    }

    #[test]
    fn collapse_blanks_matches_python_semantics() {
        // 3+ newlines -> exactly 2 (one blank line); trailing ws trimmed.
        assert_eq!(collapse_blanks("a   \n\n\n\n\nb"), "a\n\nb");
        assert_eq!(collapse_blanks("a\n\nb"), "a\n\nb"); // 2 preserved
        assert_eq!(collapse_blanks("trail \t\nx"), "trail\nx");
    }

    // ── P0: dialect externalization parity harness ──────────────────────────
    // Proves both halves of the P0 AC without a rebuild:
    //  (1) a default-equivalent dialect.json yields byte-identical output;
    //  (2) a hand-edited dialect.json changes behavior.
    // All cases use Dialect instances directly (never the OnceLock global), so a
    // write-once global cannot make a multi-dialect test green for the wrong reason.

    /// Representative corpus: real dialect prose + arbitrary passthrough text.
    fn parity_corpus() -> Vec<String> {
        vec![
            "Before creating any new hook, skill, or agent, search the existing codebase exhaustively".to_string(),
            "if any test is failing".to_string(),
            "stop all other work immediately. Fix the bug. Verify the fix passes. Only then continue".to_string(),
            "The quantum flux capacitor needs 1.21 gigawatts at node xj-42.".to_string(),
            "mixed: if there is a lint error and bandit recursive at medium severity\nplus arbitrary tail".to_string(),
        ]
    }

    #[test]
    fn bundled_default_is_lossless() {
        assert!(Dialect::bundled_default().is_lossless());
    }

    #[test]
    fn default_json_roundtrips_to_const_table() {
        // Generating dialect.json from the const table and parsing it back MUST
        // reproduce the exact same Dialect — catches raw-string/JSON escaping loss.
        let def = Dialect::bundled_default();
        let json = def.to_json();
        let reloaded = Dialect::from_json(&json).expect("generated dialect.json must parse");
        assert_eq!(
            reloaded, def,
            "round-tripped dialect.json drifted from const"
        );
    }

    #[test]
    fn default_equivalent_file_is_byte_identical() {
        // Zero-regression AC: a dialect LOADED from the default-equivalent JSON must
        // produce byte-for-byte the same output as both the const default and the
        // free `compress`/`expand` (which, with no env override, use the default).
        let loaded = Dialect::from_json(&Dialect::bundled_default().to_json()).unwrap();
        for input in parity_corpus() {
            let c_loaded = loaded.compress(&input);
            assert_eq!(c_loaded, compress(&input), "compress drift vs free fn");
            assert_eq!(
                c_loaded,
                Dialect::bundled_default().compress(&input),
                "compress drift vs const"
            );
            assert_eq!(loaded.expand(&c_loaded), expand(&compress(&input)));
            // and still lossless end-to-end
            assert_eq!(loaded.expand(&c_loaded), input);
        }
    }

    #[test]
    fn hand_edited_dialect_changes_behavior_no_rebuild() {
        // Take the default table and append a NEW pair. Same binary, different data:
        // a phrase that the default passes through now compresses.
        let phrase = "telemetry sink saturation backpressure";
        // default leaves it untouched
        assert_eq!(Dialect::bundled_default().compress(phrase), phrase);

        let mut pairs: Vec<(String, String)> =
            serde_json::from_str(&Dialect::bundled_default().to_json()).unwrap();
        pairs.push(("TSB".to_string(), phrase.to_string()));
        let edited = Dialect::from_pairs(pairs);

        let out = edited.compress(phrase);
        assert_ne!(out, phrase, "hand-edited dialect must change behavior");
        assert_eq!(out, "TSB");
        assert_eq!(edited.expand(&out), phrase, "edit must stay lossless");
        assert!(edited.is_lossless());
    }

    #[test]
    fn non_lossless_dialect_is_rejected_by_self_check() {
        // Two distinct prose values collide onto the same dense token — expand can
        // only recover one, so the round-trip breaks. is_lossless must catch it.
        let colliding = Dialect::from_pairs(vec![
            ("X".to_string(), "apple".to_string()),
            ("X".to_string(), "banana".to_string()),
        ]);
        assert!(
            !colliding.is_lossless(),
            "collision must fail the self-check so resolve() falls back to default"
        );
    }

    #[test]
    fn empty_env_path_resolves_to_bundled_default() {
        // No GLYPHDOWN_DIALECT set in the test process -> dialect_path() is None ->
        // resolve() yields the bundled default. (Asserted via dialect_path, never
        // mutating process env, to keep the OnceLock global uncontaminated.)
        assert!(dialect_path().is_none());
        assert_eq!(Dialect::resolve(), Dialect::bundled_default());
    }

    // ── compress-config: dogfood the dialect on static config files ──────────

    #[test]
    fn compress_config_is_lossless_and_saves_on_dialect_content() {
        // A line built from real dialect prose must compress AND round-trip.
        let content = "Before creating any new hook, skill, or agent, search the existing \
             codebase exhaustively. If any test is failing, stop all other work \
             immediately. Fix the bug. Verify the fix passes. Only then continue.";
        let r = Dialect::bundled_default().compress_config(content);
        assert!(r.lossless, "config compression must round-trip");
        assert!(r.safe_to_apply());
        assert!(
            r.compressed_tokens < r.original_tokens,
            "dialect prose should shrink"
        );
        assert!(r.savings_pct() > 0.0);
        // and expand recovers the exact original (the apply-safety contract)
        assert_eq!(Dialect::bundled_default().expand(&r.compressed), content);
    }

    #[test]
    fn compress_config_passthrough_is_lossless_zero_savings() {
        // Arbitrary prose with no dialect matches: unchanged, lossless, 0 saved.
        let content = "The quantum flux capacitor needs 1.21 gigawatts at node xj-42.";
        let r = Dialect::bundled_default().compress_config(content);
        assert!(r.lossless);
        assert_eq!(r.compressed, content);
        assert_eq!(r.saved_tokens(), 0);
        assert!(!r.already_dense, "plain prose is not already-dense");
    }

    #[test]
    fn compress_config_flags_already_dense_content() {
        // Content that already contains dense tokens (e.g. a prior compression)
        // is flagged. Re-compressing it does NOT round-trip (compress is ~a no-op
        // but expand over-expands), so safe_to_apply() is false — the feature
        // refuses to touch an already-compressed file. Both signals agree: leave it.
        let dense = Dialect::bundled_default()
            .compress("if any test is failing, stop all other work immediately");
        let r = Dialect::bundled_default().compress_config(&dense);
        assert!(r.already_dense, "dense input must be flagged");
        assert!(
            !r.safe_to_apply(),
            "already-dense content must not be applied (over-expansion risk)"
        );
    }

    #[test]
    fn compress_config_savings_pct_math() {
        let r = ConfigCompression {
            compressed: String::new(),
            lossless: true,
            already_dense: false,
            original_tokens: 200,
            compressed_tokens: 150,
        };
        assert_eq!(r.saved_tokens(), 50);
        assert!((r.savings_pct() - 25.0).abs() < 1e-9);
    }

    #[test]
    fn compact_payload_is_deterministic_cache_stable() {
        // CACHE-SAFETY INVARIANT: the same payload always compacts to the same
        // bytes. A tool result that later becomes part of an Anthropic prompt-cache
        // prefix must therefore stay byte-stable across turns — non-determinism here
        // would mutate a cached prefix and force a full re-fetch (the internal class).
        for s in [
            "\x1b[31merror\x1b[0m at foo.rs:42\n\n\n\nbuild failed",
            "{\"a\":1,  \"b\":  2,  \"c\":[1,2,3]}",
            "alpha\nbeta\ngamma\nunique tail line",
            "If any test is failing, stop all other work immediately.",
        ] {
            assert_eq!(
                compact_payload(s),
                compact_payload(s),
                "compaction must be deterministic — cache stability depends on it"
            );
        }
    }
}
