//! Heading-aware Markdown chunking with token-budgeted sub-splitting.
//!
//! Split a note on H1–H3 headings into sections; each section carries its heading-path breadcrumb
//! (e.g. "H1 > H2"). A section that fits the token budget becomes one chunk. A section over budget is
//! sub-split on paragraph boundaries (greedily packed, with a re-prepended overlap tail) so chunks
//! stay sized to the embedder's context window (G20). H4–H6 headings stay inline as body text. A note
//! with no headings still indexes fine (G3) — short ones as a single chunk, long ones sub-split.
//!
//! **Fenced code blocks are atomic:** because we now sub-split *within* a section, the splitter tracks
//! prose vs. fenced-code segments and never cuts inside a code block — an over-budget block is emitted
//! whole as its own chunk.
//!
//! `chunk_id = sha256(path + "#" + ord)` is stable for a stable file, so re-chunking an unchanged
//! file yields identical ids.

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

use crate::util::sha256_hex;

/// Target chunk size in *estimated* tokens, sized well under EmbeddingGemma's 2048-token context
/// (G20). Apes qmd's ~900-token chunks.
const CHUNK_BUDGET_TOKENS: usize = 900;
/// Overlap (estimated tokens) re-prepended to each continuation sub-chunk for retrieval continuity.
const CHUNK_OVERLAP_TOKENS: usize = 128;

/// Dep-free token estimate. ~3.5 chars/token is conservative (i.e. over-counts) for token-dense
/// technical content, keeping us safely under the hard context limit without a tokenizer in the hot
/// path (G11).
fn estimate_tokens(s: &str) -> usize {
    ((s.chars().count() as f32) / 3.5).ceil() as usize
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: String,
    pub ord: usize,
    pub heading_path: String,
    pub body: String,
}

/// A run of section content: prose can be split at paragraph boundaries; fenced code is atomic.
#[derive(Debug, Clone)]
enum Seg {
    Prose(String),
    Code(String),
}

fn level_num(l: HeadingLevel) -> usize {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Return the note body with a leading YAML frontmatter block (`---` … `---`) removed.
fn strip_frontmatter(text: &str) -> String {
    let mut lines = text.lines();
    if lines.next() == Some("---") {
        let mut body = Vec::new();
        let mut closed = false;
        for line in lines {
            if !closed {
                if line.trim_end() == "---" {
                    closed = true;
                }
                continue;
            }
            body.push(line);
        }
        if closed {
            return body.join("\n");
        }
    }
    text.to_string()
}

/// Split `text` (the note at vault-relative `path`) into heading-aware, budget-sized chunks.
pub fn chunk_markdown(path: &str, text: &str) -> Vec<Chunk> {
    // Don't index YAML frontmatter (created/status/source/…) as note content.
    let md = strip_frontmatter(text);

    // Heading breadcrumb stack of (level, text) for levels 1..=3.
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut sections: Vec<(String, Vec<Seg>)> = Vec::new(); // (heading_path, segments)
    let mut segs: Vec<Seg> = Vec::new();
    let mut prose = String::new();
    let mut code = String::new();
    let mut heading_buf = String::new();
    let mut in_heading: Option<usize> = None;
    let mut in_code = false;

    let heading_path = |stack: &[(usize, String)]| -> String {
        stack
            .iter()
            .map(|(_, t)| t.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" > ")
    };

    // Flush the current prose buffer into the segment list (dropping a whitespace-only buffer).
    fn flush_prose(segs: &mut Vec<Seg>, prose: &mut String) {
        if !prose.trim().is_empty() {
            segs.push(Seg::Prose(std::mem::take(prose)));
        } else {
            prose.clear();
        }
    }

    for ev in Parser::new(&md) {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                in_heading = Some(level_num(level));
                heading_buf.clear();
            }
            Event::End(TagEnd::Heading(level)) => {
                let lvl = level_num(level);
                in_heading = None;
                let title = heading_buf.trim().to_string();
                if lvl <= 3 {
                    // Close the current section, then update the breadcrumb and open a new one.
                    flush_prose(&mut segs, &mut prose);
                    sections.push((heading_path(&stack), std::mem::take(&mut segs)));
                    stack.retain(|(l, _)| *l < lvl);
                    stack.push((lvl, title));
                } else {
                    // H4–H6: keep the heading text inline in the body.
                    prose.push_str(&title);
                    prose.push('\n');
                }
            }
            Event::Start(Tag::CodeBlock(_)) => {
                flush_prose(&mut segs, &mut prose);
                in_code = true;
                code.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                if !code.trim().is_empty() {
                    segs.push(Seg::Code(std::mem::take(&mut code)));
                } else {
                    code.clear();
                }
            }
            Event::Text(t) => {
                if in_heading.is_some() {
                    heading_buf.push_str(&t);
                } else if in_code {
                    code.push_str(&t);
                } else {
                    prose.push_str(&t);
                }
            }
            // Inline code (backtick span) is always prose, never inside a fenced block.
            Event::Code(t) => {
                if in_heading.is_some() {
                    heading_buf.push_str(&t);
                } else {
                    prose.push_str(&t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if in_code {
                    code.push('\n');
                } else {
                    prose.push('\n');
                }
            }
            // Paragraph/rule end → a blank line, so paragraphs are separable when sub-splitting.
            Event::End(TagEnd::Paragraph) | Event::Rule => prose.push_str("\n\n"),
            _ => {}
        }
    }
    flush_prose(&mut segs, &mut prose);
    sections.push((heading_path(&stack), segs));

    let mut chunks = Vec::new();
    for (heading_path, segs) in sections {
        for body in pack_section(&segs) {
            let body = body.trim().to_string();
            if body.is_empty() && heading_path.is_empty() {
                continue; // skip empty preamble (a heading-only section keeps its empty chunk)
            }
            let ord = chunks.len();
            chunks.push(Chunk {
                id: sha256_hex(format!("{path}#{ord}").as_bytes()),
                ord,
                heading_path: heading_path.clone(),
                body,
            });
        }
    }
    chunks
}

/// Pack a section's segments into chunk bodies, each ≈ ≤ `CHUNK_BUDGET_TOKENS` (an oversize fenced
/// code block is the one allowed exception — kept atomic). Returns at least one (possibly empty) body
/// so a heading-only section still yields a chunk.
fn pack_section(segs: &[Seg]) -> Vec<String> {
    let budget = CHUNK_BUDGET_TOKENS;
    let pieces = to_pieces(segs, budget);
    if pieces.is_empty() {
        return vec![String::new()];
    }

    // `cur` holds only *new* content; `overlap` is the tail carried from the previous chunk and is
    // prepended at assembly time. Keeping them separate means we never emit a pure-overlap chunk.
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut overlap = String::new();
    for p in &pieces {
        let oversize = estimate_tokens(p) > budget; // only an atomic code block can be oversize
        let would = estimate_tokens(&overlap) + estimate_tokens(&cur) + estimate_tokens(p);
        if !cur.is_empty() && (oversize || would > budget) {
            out.push(assemble(&overlap, &cur));
            overlap = overlap_tail(out.last().unwrap());
            cur.clear();
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(p);
        if oversize {
            out.push(assemble(&overlap, &cur));
            overlap = overlap_tail(out.last().unwrap());
            cur.clear();
        }
    }
    if !cur.is_empty() {
        out.push(assemble(&overlap, &cur));
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Join an optional overlap tail in front of the current chunk content (both trimmed).
fn assemble(overlap: &str, cur: &str) -> String {
    if overlap.trim().is_empty() {
        cur.trim().to_string()
    } else {
        format!("{}\n\n{}", overlap.trim(), cur.trim())
    }
}

/// Flatten segments into packable pieces, each ≤ budget (except an atomic code block kept whole).
fn to_pieces(segs: &[Seg], budget: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    for seg in segs {
        match seg {
            Seg::Code(t) => {
                let t = t.trim_end();
                if !t.is_empty() {
                    pieces.push(t.to_string()); // kept whole even if over budget
                }
            }
            Seg::Prose(t) => {
                for para in t.split("\n\n") {
                    let para = para.trim();
                    if para.is_empty() {
                        continue;
                    }
                    if estimate_tokens(para) <= budget {
                        pieces.push(para.to_string());
                    } else {
                        pieces.extend(hard_split_words(para, budget));
                    }
                }
            }
        }
    }
    pieces
}

/// Greedily split an over-budget paragraph at whitespace into ≤ budget word-runs.
fn hard_split_words(para: &str, budget: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in para.split_whitespace() {
        if !cur.is_empty() && estimate_tokens(&cur) + estimate_tokens(word) + 1 > budget {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The trailing ~`CHUNK_OVERLAP_TOKENS` of `prev`, snapped to a whitespace boundary so we don't start
/// a continuation chunk mid-word.
fn overlap_tail(prev: &str) -> String {
    let overlap_chars = ((CHUNK_OVERLAP_TOKENS as f32) * 3.5) as usize;
    let total = prev.chars().count();
    if total <= overlap_chars {
        return prev.to_string();
    }
    let start = total - overlap_chars;
    let tail: String = prev.chars().skip(start).collect();
    // Snap forward to the first whitespace so we begin on a word boundary.
    match tail.find(char::is_whitespace) {
        Some(i) => tail[i..].trim_start().to_string(),
        None => tail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_headings_yields_one_chunk() {
        let c = chunk_markdown("a.md", "just a bare idea, no frontmatter");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].heading_path, "");
        assert!(c[0].body.contains("bare idea"));
    }

    #[test]
    fn headings_build_breadcrumbs_and_keep_code() {
        let md = "# Title\nintro\n## Sub\n```rust\nlet x = 1;\n```\nmore\n";
        let c = chunk_markdown("a.md", md);
        assert!(c.len() >= 2);
        let sub = c.iter().find(|c| c.heading_path == "Title > Sub").unwrap();
        assert!(sub.body.contains("let x = 1;"));
    }

    #[test]
    fn stable_ids_for_stable_file() {
        let md = "# A\nx\n# B\ny\n";
        let a = chunk_markdown("p.md", md);
        let b = chunk_markdown("p.md", md);
        assert_eq!(
            a.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
            b.iter().map(|c| c.id.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn frontmatter_is_not_indexed() {
        let md = "---\ncreated: 2026-05-29T18:02\nstatus: inbox\nsource: chat\n---\n\n# Title\n\nbody text\n";
        let c = chunk_markdown("p.md", md);
        let all: String = c
            .iter()
            .map(|c| format!("{} {}", c.heading_path, c.body))
            .collect();
        assert!(
            !all.contains("status"),
            "frontmatter leaked into chunks: {all}"
        );
        assert!(
            !all.contains("created"),
            "frontmatter leaked into chunks: {all}"
        );
        assert!(all.contains("Title"));
        assert!(all.contains("body text"));
    }

    #[test]
    fn long_headingless_note_splits_into_multiple_chunks() {
        // ~30 paragraphs of ~200 chars each (~6000 chars ≈ 1700 tokens) — over the ~900 budget.
        let para = "This is a sentence of reasonably typical prose that carries some weight and \
                    fills out a paragraph so the estimator counts a good number of tokens here.";
        let md = (0..30).map(|_| para).collect::<Vec<_>>().join("\n\n");
        let c = chunk_markdown("long.md", &md);
        assert!(c.len() > 1, "expected multiple chunks, got {}", c.len());
        // Every prose chunk stays within budget.
        for ch in &c {
            assert!(
                estimate_tokens(&ch.body) <= CHUNK_BUDGET_TOKENS + CHUNK_OVERLAP_TOKENS + 5,
                "chunk over budget: {} tokens",
                estimate_tokens(&ch.body)
            );
        }
        // Ids are dense + stable across a second run.
        let again = chunk_markdown("long.md", &md);
        assert_eq!(c.len(), again.len());
        assert_eq!(c[0].id, again[0].id);
    }

    #[test]
    fn oversize_code_block_stays_in_one_chunk() {
        // A fenced block well over budget, between prose, must not be split.
        let big_code = (0..400)
            .map(|i| format!("    let v{i} = compute_value({i}) + offset; // line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let md =
            format!("# Code\n\nbefore the block\n\n```rust\n{big_code}\n```\n\nafter the block\n");
        let c = chunk_markdown("code.md", &md);
        // Exactly one chunk contains the first and last lines of the block — i.e. it wasn't cut.
        let with_first: Vec<_> = c.iter().filter(|ch| ch.body.contains("let v0 =")).collect();
        assert_eq!(
            with_first.len(),
            1,
            "code start appears in >1 chunk (was split)"
        );
        assert!(
            with_first[0].body.contains("let v399 ="),
            "code block was split across chunks"
        );
    }
}
