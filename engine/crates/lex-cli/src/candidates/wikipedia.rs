//! Mine extras candidates from a Wikipedia XML dump.
//!
//! Lazy "surface-first" pipeline:
//!
//! 1. Stream-decompress the dump (`.xml.bz2` or `.xml`) line by line.
//! 2. Inside `<text>...</text>` regions, extract maximal kanji runs.
//! 3. Frequency-count surfaces (HashMap).
//! 4. (Caller) diff against the build dict's surface set; surviving surfaces
//!    are real Mozc gaps.
//!
//! No morphological analysis here — that step happens later (only for the
//! diffed gap candidates), since reading assignment is the expensive part.
//! See `feedback_extras_promotion.md` for why this approach was chosen
//! over Sudachi/Wikidata seed sources.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use bzip2::read::MultiBzDecoder;

use super::CandidateError;

/// Minimum kanji-run length to count. Single-char surfaces are dominated by
/// fragments of compounds (e.g. の境内 → 境 + 内 fragments) and add noise.
pub const MIN_SURFACE_CHARS: usize = 2;

/// Maximum length to count. Long runs (>20 chars) tend to be wiki-markup
/// artifacts (concatenated table cells, broken templates).
pub const MAX_SURFACE_CHARS: usize = 20;

/// Frequency floor when emitting candidates. count<3 is heavy long-tail
/// noise — single article typos, OCR errors in references, etc.
pub const DEFAULT_MIN_FREQ: u32 = 3;

/// Stream-extract kanji-run frequencies from a Wikipedia dump.
///
/// `dump_path` may be `.xml.bz2` (decompressed on the fly) or already-
/// decompressed `.xml`. Detection is by extension — explicit, no magic-byte
/// guessing.
pub fn extract_kanji_freqs(dump_path: &Path) -> Result<HashMap<String, u32>, CandidateError> {
    let file = File::open(dump_path)?;
    let reader: Box<dyn Read> = if dump_path.extension().and_then(|s| s.to_str()) == Some("bz2") {
        // MultiBzDecoder handles concatenated bz2 streams (Wikipedia dumps
        // are sometimes split into multiple bz2 blocks).
        Box::new(MultiBzDecoder::new(file))
    } else {
        Box::new(file)
    };
    extract_kanji_freqs_from_reader(BufReader::with_capacity(1 << 20, reader), true)
}

/// Pure-stream variant of `extract_kanji_freqs`. Public-in-crate so tests
/// can exercise the parse loop without touching the filesystem (avoids
/// flaky tempfile races in parallel runs).
pub(crate) fn extract_kanji_freqs_from_reader<R: BufRead>(
    reader: R,
    progress: bool,
) -> Result<HashMap<String, u32>, CandidateError> {
    let mut freqs: HashMap<String, u32> = HashMap::new();
    let mut in_text = false;
    let mut buf = String::new();
    let mut pages_seen: u64 = 0;
    let mut pages_scanned: u64 = 0;
    let mut bytes_seen: u64 = 0;
    let mut last_progress = std::time::Instant::now();
    // Wikitext template depth across line boundaries. Templates `{{...}}`
    // contain field-name boilerplate (`乗車人員`, `駅構造`, `所属路線`...)
    // that dominates top-frequency noise. Skip everything inside them.
    // References `<ref>...</ref>` similarly contain citation strings.
    let mut tmpl_depth: i32 = 0;
    let mut in_ref = false;
    // Per-page scratch state. <ns> arrives before <text> in the dump format,
    // so we know whether to scan this page's text by the time we see it.
    // Default to article (true) so older dumps without an explicit <ns> tag
    // still get scanned.
    let mut current_page_is_article = true;

    for line_res in reader.lines() {
        let line = line_res?;
        bytes_seen += line.len() as u64 + 1;

        // Reset at each <page> boundary so a non-article page doesn't
        // poison the next page when no explicit <ns> is provided.
        // Also reset markup-skip state: a page with unbalanced `{{...`,
        // `<ref ...`, or `<text` (without matching close) would otherwise
        // drag its open-block state into the next page and silently skip
        // everything that follows. `in_text` is reset here too — a stray
        // self-closing/unclosed `<text` on a non-article page must not
        // make us treat subsequent XML metadata of the NEXT page as prose.
        if line.contains("<page>") {
            current_page_is_article = true;
            tmpl_depth = 0;
            in_ref = false;
            in_text = false;
            buf.clear();
        }
        // Parse <ns>NUM</ns>. Filter to ns=0 (main article namespace).
        // Skips Wikipedia: / User: / File: / Template: / Category: pages
        // whose template-arg names and file-upload logs dominate top-
        // frequency noise.
        if let Some(start) = line.find("<ns>") {
            if let Some(end) = line[start..].find("</ns>") {
                let raw = &line[start + 4..start + end];
                let ns: i32 = raw.trim().parse().unwrap_or(-1);
                current_page_is_article = ns == 0;
            }
        }

        // Detect the `<text` opening tag, with three flavours to handle:
        //   `<text>body</text>`      single line (close after open)
        //   `<text>body`             open continues to next line
        //   `<text ... />`           self-closing, empty body (rare but
        //                            present in real dumps for redirect /
        //                            stub pages)
        let text_open = line.find("<text");
        let text_close = line.find("</text>");
        let text_self_closing = text_open.is_some_and(|o| {
            // Self-closing iff the first `>` after `<text` is preceded by `/`.
            line[o..]
                .find('>')
                .is_some_and(|rel| rel > 0 && line.as_bytes()[o + rel - 1] == b'/')
        });
        let scan_slice: &str = match (in_text, text_open, text_close, text_self_closing) {
            (false, Some(_), _, true) => {
                // Self-closing `<text ... />` — page seen, nothing to scan.
                pages_seen += 1;
                if current_page_is_article {
                    pages_scanned += 1;
                }
                ""
            }
            (false, Some(o), Some(c), false) if c > o => {
                // Whole text on one line.
                pages_seen += 1;
                if current_page_is_article {
                    pages_scanned += 1;
                }
                let after_open = &line[o..];
                let body_start = after_open.find('>').map(|p| o + p + 1).unwrap_or(o);
                &line[body_start..c]
            }
            (false, Some(o), None, false) => {
                in_text = true;
                pages_seen += 1;
                if current_page_is_article {
                    pages_scanned += 1;
                }
                let after_open = &line[o..];
                let body_start = after_open.find('>').map(|p| o + p + 1).unwrap_or(o);
                &line[body_start..]
            }
            (true, _, Some(c), _) => {
                in_text = false;
                &line[..c]
            }
            (true, _, None, _) => &line[..],
            _ => "",
        };

        if !scan_slice.is_empty() && current_page_is_article {
            scan_prose_kanji_runs(
                scan_slice,
                &mut buf,
                &mut freqs,
                &mut tmpl_depth,
                &mut in_ref,
            );
        }

        if progress && last_progress.elapsed().as_secs() >= 10 {
            eprintln!(
                "  ... {} pages ({} articles scanned), ~{} MB, {} surfaces",
                pages_seen,
                pages_scanned,
                bytes_seen >> 20,
                freqs.len()
            );
            last_progress = std::time::Instant::now();
        }
    }

    if progress {
        eprintln!(
            "Done. {} pages, {} articles scanned, ~{} MB, {} unique surfaces",
            pages_seen,
            pages_scanned,
            bytes_seen >> 20,
            freqs.len()
        );
    }
    Ok(freqs)
}

/// Wrapper that skips wikitext template (`{{...}}`) and `<ref>` blocks before
/// counting kanji runs. State is carried across calls so multi-line templates
/// stay closed.
///
/// `tmpl_depth` increases on `{{`, decreases on `}}`. `in_ref` toggles on
/// `<ref` / `</ref>`. Outside-block byte ranges are passed by reference
/// (`&s[prose_start..i]`) directly to `scan_kanji_runs` whenever a block
/// opens, closes, or the slice ends — no intermediate copy of the prose
/// itself; only the per-run `buf` inside `scan_kanji_runs` is reused.
fn scan_prose_kanji_runs(
    s: &str,
    buf: &mut String,
    freqs: &mut HashMap<String, u32>,
    tmpl_depth: &mut i32,
    in_ref: &mut bool,
) {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut prose_start = 0; // start of the current prose run (when not inside a block)
    // UTF-8 safety: this loop is byte-indexed, but `&s[prose_start..i]`
    // slicing is always at a char boundary because every advance of
    // either index happens just past an ASCII delimiter byte (`{`, `}`,
    // `<`, `>`, `/`, or whitespace — all 0x00-0x7F). UTF-8 continuation
    // bytes are strictly 0x80-0xBF, so multi-byte chars (kanji etc.)
    // cannot contribute a byte that matches any of our ASCII branches.
    while i < bytes.len() {
        // Inline match on 2-byte ASCII pairs and `<ref` / `</ref>` headers.
        // Using as_bytes lets us peek without UTF-8 decoding overhead;
        // kanji are multi-byte but we only branch on ASCII patterns.
        let in_block = *tmpl_depth > 0 || *in_ref;
        let b = bytes[i];

        if !in_block && b == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // flush prose
            if i > prose_start {
                scan_kanji_runs(&s[prose_start..i], buf, freqs);
            }
            *tmpl_depth += 1;
            i += 2;
            prose_start = i;
            continue;
        }
        if *tmpl_depth > 0 && b == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            *tmpl_depth += 1;
            i += 2;
            continue;
        }
        if *tmpl_depth > 0 && b == b'}' && i + 1 < bytes.len() && bytes[i + 1] == b'}' {
            *tmpl_depth -= 1;
            i += 2;
            if *tmpl_depth == 0 {
                prose_start = i;
            }
            continue;
        }
        if !in_block && b == b'<' && is_ref_open(&s[i..], bytes, i) {
            // Self-closing `<ref ... />` is one shot; full `<ref>...</ref>`
            // is multi-token. Cheaply check the next `>`.
            if i > prose_start {
                scan_kanji_runs(&s[prose_start..i], buf, freqs);
            }
            // Find end of opening tag.
            if let Some(rel) = s[i..].find('>') {
                let close = i + rel;
                // Self-closing if char before `>` is `/`.
                if close > 0 && bytes[close - 1] == b'/' {
                    i = close + 1;
                    prose_start = i;
                    continue;
                }
                *in_ref = true;
                i = close + 1;
                prose_start = i;
                continue;
            } else {
                // Tag continues to next line; assume opening
                *in_ref = true;
                i = bytes.len();
                prose_start = i;
                break;
            }
        }
        if *in_ref && b == b'<' && s[i..].starts_with("</ref>") {
            *in_ref = false;
            i += 6;
            prose_start = i;
            continue;
        }

        i += 1;
    }
    if !*in_ref && *tmpl_depth == 0 && prose_start < bytes.len() {
        scan_kanji_runs(&s[prose_start..], buf, freqs);
    }
}

/// Distinguish `<ref>` / `<ref ...>` / `<ref/>` from `<references>` and
/// `<refer...>` etc. `<references>` closes with `</references>` (not
/// `</ref>`), so naive `starts_with("<ref")` traps `in_ref` permanently
/// — the rest of the page (and worse, subsequent pages) silently drop.
fn is_ref_open(slice: &str, bytes: &[u8], i: usize) -> bool {
    if !slice.starts_with("<ref") {
        return false;
    }
    // 4 = len("<ref"). Char after "<ref" must terminate the tag name.
    match bytes.get(i + 4) {
        Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'>') | Some(b'/') => true,
        // EOF after "<ref" — treat as opening (line break inside attrs).
        None => true,
        _ => false,
    }
}

/// Scan one slice for maximal kanji runs and bump frequencies.
///
/// `buf` is reused across calls so we don't reallocate per run.
fn scan_kanji_runs(s: &str, buf: &mut String, freqs: &mut HashMap<String, u32>) {
    buf.clear();
    let mut char_count: usize = 0;
    for ch in s.chars() {
        if is_kanji(ch) {
            buf.push(ch);
            char_count += 1;
        } else if !buf.is_empty() {
            if (MIN_SURFACE_CHARS..=MAX_SURFACE_CHARS).contains(&char_count) {
                // Avoid cloning the working buffer when the entry is fresh:
                // entry().or_insert(buf.clone()) and entry-API patterns both
                // require an owned key. Looking up first lets us only clone
                // on insert, which is the cold path once vocab saturates.
                if let Some(v) = freqs.get_mut(buf.as_str()) {
                    *v = v.saturating_add(1);
                } else {
                    freqs.insert(buf.clone(), 1);
                }
            }
            buf.clear();
            char_count = 0;
        }
    }
    if !buf.is_empty() && (MIN_SURFACE_CHARS..=MAX_SURFACE_CHARS).contains(&char_count) {
        if let Some(v) = freqs.get_mut(buf.as_str()) {
            *v = v.saturating_add(1);
        } else {
            freqs.insert(buf.clone(), 1);
        }
    }
}

/// CJK Unified Ideographs (U+4E00–U+9FFF) plus iteration mark 々 (U+3005).
/// Excludes Extension A/B (rare archaic chars dominate noise) and katakana
/// ヶ (typically a counter, not a content char).
fn is_kanji(ch: char) -> bool {
    matches!(ch, '\u{4E00}'..='\u{9FFF}' | '\u{3005}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_extracts_maximal_kanji_runs() {
        let mut buf = String::new();
        let mut f = HashMap::new();
        scan_kanji_runs("これは日本語の文章です", &mut buf, &mut f);
        // 日本語 (3 chars) and 文章 (2 chars) qualify.
        // 「これは / の / です」are hiragana — skipped.
        assert_eq!(f.get("日本語"), Some(&1));
        assert_eq!(f.get("文章"), Some(&1));
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn scan_drops_single_char_surfaces() {
        let mut buf = String::new();
        let mut f = HashMap::new();
        // 「私」と「本」は 1 字 → MIN_SURFACE_CHARS=2 で skip
        scan_kanji_runs("私の本", &mut buf, &mut f);
        assert!(f.is_empty());
    }

    #[test]
    fn scan_drops_oversized_runs() {
        let mut buf = String::new();
        let mut f = HashMap::new();
        let huge: String = "亜".repeat(MAX_SURFACE_CHARS + 1);
        scan_kanji_runs(&huge, &mut buf, &mut f);
        assert!(f.is_empty());
        // Boundary: exactly MAX_SURFACE_CHARS should survive.
        let edge: String = "亜".repeat(MAX_SURFACE_CHARS);
        let mut f2 = HashMap::new();
        scan_kanji_runs(&edge, &mut buf, &mut f2);
        assert_eq!(f2.get(edge.as_str()), Some(&1));
    }

    #[test]
    fn scan_treats_iter_mark_as_kanji() {
        let mut buf = String::new();
        let mut f = HashMap::new();
        // 「人々」は 々 を含む 2-char surface → keep
        scan_kanji_runs("人々が集まる", &mut buf, &mut f);
        assert_eq!(f.get("人々"), Some(&1));
    }

    #[test]
    fn scan_accumulates_frequency() {
        let mut buf = String::new();
        let mut f = HashMap::new();
        scan_kanji_runs("日本語と日本語と日本語", &mut buf, &mut f);
        assert_eq!(f.get("日本語"), Some(&3));
    }

    #[test]
    fn scan_emits_run_at_eol() {
        // Run that runs to end-of-string (no trailing non-kanji) must still
        // be flushed.
        let mut buf = String::new();
        let mut f = HashMap::new();
        scan_kanji_runs("文末は日本語", &mut buf, &mut f);
        assert_eq!(f.get("文末"), Some(&1));
        assert_eq!(f.get("日本語"), Some(&1));
    }

    #[test]
    fn is_kanji_classifies_correctly() {
        assert!(is_kanji('日'));
        assert!(is_kanji('語'));
        assert!(is_kanji('々'));
        assert!(!is_kanji('あ')); // hiragana
        assert!(!is_kanji('ア')); // katakana
        assert!(!is_kanji('A')); // ascii
        assert!(!is_kanji('1')); // digit
    }
}

#[cfg(test)]
mod prose_tests {
    use super::*;
    use std::collections::HashMap;

    fn scan_one(s: &str) -> HashMap<String, u32> {
        let mut buf = String::new();
        let mut f = HashMap::new();
        let mut depth = 0;
        let mut in_ref = false;
        scan_prose_kanji_runs(s, &mut buf, &mut f, &mut depth, &mut in_ref);
        assert_eq!(depth, 0);
        assert!(!in_ref);
        f
    }

    #[test]
    fn template_block_is_skipped() {
        // 通常文章 (4 kanji), then a template block, then 続きの文章 — the
        // hiragana き / の inside the tail break the run, so only "文章"
        // survives from the tail. The template's "乗車人員" must NOT count.
        let f = scan_one("通常文章{{infobox|乗車人員=12345}}続きの文章");
        assert_eq!(f.get("通常文章"), Some(&1));
        assert_eq!(f.get("文章"), Some(&1));
        assert!(!f.contains_key("乗車人員"));
    }

    #[test]
    fn nested_template_closes_correctly() {
        let f = scan_one("外側{{a|{{b|内側}}|x}}終端文章");
        assert_eq!(f.get("外側"), Some(&1));
        assert_eq!(f.get("終端文章"), Some(&1));
        // Inside nested template — must not be counted.
        assert!(!f.contains_key("内側"));
    }

    #[test]
    fn ref_block_is_skipped() {
        // 本文章 (3-kanji) + ref block + 続き文章. After ref skip, き breaks
        // the tail run, so only "文章" survives from the trailer.
        let f = scan_one("本文章<ref>引用元の出典</ref>続き文章");
        assert_eq!(f.get("本文章"), Some(&1));
        assert_eq!(f.get("文章"), Some(&1));
        assert!(!f.contains_key("出典"));
        assert!(!f.contains_key("引用元"));
    }

    #[test]
    fn self_closing_ref_is_handled() {
        let f = scan_one("先頭文章<ref name=\"x\" />終端文章");
        assert_eq!(f.get("先頭文章"), Some(&1));
        assert_eq!(f.get("終端文章"), Some(&1));
    }

    #[test]
    fn template_state_persists_across_slices() {
        let mut buf = String::new();
        let mut f = HashMap::new();
        let mut depth = 0;
        let mut in_ref = false;
        scan_prose_kanji_runs(
            "普通文{{tmpl|内容",
            &mut buf,
            &mut f,
            &mut depth,
            &mut in_ref,
        );
        assert_eq!(depth, 1);
        scan_prose_kanji_runs(
            "続き|更に}}終了文章",
            &mut buf,
            &mut f,
            &mut depth,
            &mut in_ref,
        );
        assert_eq!(depth, 0);
        assert_eq!(f.get("普通文"), Some(&1));
        assert_eq!(f.get("終了文章"), Some(&1));
        assert!(!f.contains_key("内容"));
        assert!(!f.contains_key("更に"));
    }

    #[test]
    fn references_tag_does_not_trap_in_ref() {
        // `<references>` (and `<references/>` / `<references xml:space="..."/>`)
        // closes with `</references>`, NOT `</ref>`. Naive `<ref` matching
        // would trap in_ref true forever, silently dropping the rest of the
        // page. We must NOT enter in_ref state for this tag.
        let f = scan_one("先頭文章<references/>末尾文章");
        assert_eq!(f.get("先頭文章"), Some(&1));
        assert_eq!(f.get("末尾文章"), Some(&1));

        let f2 = scan_one("先頭文章<references>引用集</references>末尾文章");
        assert_eq!(f2.get("先頭文章"), Some(&1));
        assert_eq!(f2.get("末尾文章"), Some(&1));
        // The <references> body content here happens to look like prose
        // since we didn't treat it as a block — that's fine; we trade
        // theoretical "block body" purity for not losing the rest of the
        // page when </ref> never arrives.
    }

    #[test]
    fn page_boundary_resets_block_state() {
        // A page with unbalanced `{{...` (no closing `}}`) leaves
        // tmpl_depth > 0. The driver loop resets state at <page>
        // boundaries — verify that the SECOND page is fully scanned.
        let dump = "<page>\n<ns>0</ns>\n<text>第一段{{壊れ|未閉</text>\n</page>\n\
                    <page>\n<ns>0</ns>\n<text>第二段文章</text>\n</page>";
        let freqs = extract_kanji_freqs_from_str(dump).unwrap();
        // 第一段 must be present (scanned before the open `{{`).
        assert_eq!(freqs.get("第一段"), Some(&1));
        // 第二段文章 must be present — would be missing if state leaked.
        assert_eq!(freqs.get("第二段文章"), Some(&1));
        // The unclosed-template body must NOT leak through.
        assert!(!freqs.contains_key("未閉"));
    }

    #[test]
    fn self_closing_text_tag_is_handled() {
        // `<text ... />` (empty content, e.g. for redirect / stub pages)
        // must NOT flip in_text to true — otherwise subsequent XML metadata
        // lines of the next page would be scanned as prose.
        let dump = "<page>\n<ns>0</ns>\n<text bytes=\"0\" />\n</page>\n\
                    <page>\n<ns>0</ns>\n<title>普通記事</title>\n<text>本文文章</text>\n</page>";
        let freqs = extract_kanji_freqs_from_str(dump).unwrap();
        // 本文文章 from the second page must be counted.
        assert_eq!(freqs.get("本文文章"), Some(&1));
        // The XML metadata of page 2 (`<title>普通記事</title>`) must NOT
        // be counted as prose — would leak if in_text stuck true.
        assert!(!freqs.contains_key("普通記事"));
    }

    /// Test helper: drive the stream parser with an in-memory dump.
    /// Avoids tempfile flakiness in parallel test runs.
    fn extract_kanji_freqs_from_str(s: &str) -> Result<HashMap<String, u32>, CandidateError> {
        extract_kanji_freqs_from_reader(std::io::Cursor::new(s.as_bytes()), false)
    }
}
