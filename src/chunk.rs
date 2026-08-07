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

use crate::frontmatter::producer_search_text;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    Content,
    ProducerMetadata,
}

impl ChunkKind {
    pub fn as_i64(self) -> i64 {
        match self {
            Self::Content => 0,
            Self::ProducerMetadata => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: String,
    pub ord: usize,
    pub kind: ChunkKind,
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

/// One non-Vagus top-level field whose compact JSON value is projected into a searchable chunk
/// (ADR 0028). The original frontmatter remains untouched in the Markdown source of truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerMetadata {
    pub key: String,
    pub search_text: String,
}

/// Hand-parsed note-level frontmatter used for filters (ADR 0017) and validated producer metadata
/// search chunks (ADR 0028). Lifecycle fields remain filters only; they never become chunk text.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Frontmatter {
    /// Raw `created` value (e.g. `2026-05-29T18:02`), if present. Parsed to a unix timestamp by the
    /// caller (`index`) so this module stays free of a timezone dependency.
    pub created: Option<String>,
    /// Raw `source` value (provenance), if present. NULL `source` never matches `--source`.
    pub source: Option<String>,
    /// Non-owned top-level fields with valid compact JSON values, in source order.
    pub producer_metadata: Vec<ProducerMetadata>,
}

/// Extract indexed values from a complete leading YAML frontmatter block. `created` / `source` are
/// hand-parsed scalar filters; non-Vagus fields are accepted for search only when their one-line value
/// parses as JSON, matching the safe producer contract from ADR 0027. An absent or unclosed block
/// returns the default so parsing mirrors `strip_frontmatter` exactly without adding a YAML dependency.
pub fn parse_frontmatter(text: &str) -> Frontmatter {
    let mut parsed = Frontmatter::default();
    let mut lines = text.lines();
    if lines.next() != Some("---") {
        return parsed;
    }
    for line in lines {
        if line.trim_end() == "---" {
            return parsed;
        }
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let raw = val.trim();
        if raw.is_empty() {
            continue;
        }
        match key {
            "created" => {
                parsed
                    .created
                    .get_or_insert_with(|| raw.trim_matches(|c| c == '"' || c == '\'').to_string());
            }
            "source" => {
                parsed
                    .source
                    .get_or_insert_with(|| raw.trim_matches(|c| c == '"' || c == '\'').to_string());
            }
            _ => {
                if let Some(search_text) = producer_search_text(key, raw) {
                    parsed.producer_metadata.push(ProducerMetadata {
                        key: key.to_owned(),
                        search_text,
                    });
                }
            }
        }
    }
    Frontmatter::default()
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

/// Split `text` (the note at vault-relative `path`) into heading-aware, budget-sized content chunks,
/// followed by dedicated chunks for valid producer JSON fields (ADR 0028). Vagus lifecycle
/// frontmatter remains excluded. Metadata is kept in its own chunk kind so rerank context windows do
/// not displace neighboring note content.
pub fn chunk_markdown(path: &str, text: &str) -> Vec<Chunk> {
    let producer_metadata = parse_frontmatter(text).producer_metadata;
    // Strip the complete YAML block before ordinary Markdown chunking. Selected producer fields are
    // reintroduced only through their normalized, bounded searchable projection below.
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
    for (heading_path, segs) in &sections {
        let before = chunks.len();
        for body in pack_section(segs) {
            let body = body.trim().to_string();
            if body.is_empty() {
                continue;
            }
            push_chunk(
                &mut chunks,
                path,
                ChunkKind::Content,
                heading_path.clone(),
                body,
            );
        }
        if chunks.len() > before {
            continue; // section carried real body text
        }
        // Empty-bodied section. tantivy indexes the `heading` field for BM25 (lex.rs), so this
        // section's heading tokens are only searchable via its own heading_path. Drop it only when a
        // descendant section carries the same breadcrumb (the ancestor tokens survive there anyway);
        // a bodyless *leaf* heading (e.g. an "Open Questions" placeholder) would otherwise vanish from
        // the index, so keep it as a heading-only chunk. A truly contentless note indexes nothing.
        let covered_by_descendant = sections.iter().any(|(hp, _)| {
            hp.len() > heading_path.len()
                && hp.starts_with(heading_path.as_str())
                && (heading_path.is_empty() || hp[heading_path.len()..].starts_with(" > "))
        });
        if covered_by_descendant {
            continue;
        }
        let leaf = heading_path.rsplit(" > ").next().unwrap_or_default();
        if !leaf.is_empty() {
            push_chunk(
                &mut chunks,
                path,
                ChunkKind::Content,
                heading_path.clone(),
                leaf.to_string(),
            );
        }
    }
    // Producer metadata comes last, preserving every content chunk's historical ord/id. Each
    // top-level field is independently budgeted; a large JSON value can therefore span several
    // metadata chunks without exceeding the embedding target.
    for metadata in producer_metadata {
        let heading_path = format!("Frontmatter > {}", metadata.key);
        for body in pack_section(&[Seg::Prose(metadata.search_text)]) {
            let body = body.trim().to_owned();
            if !body.is_empty() {
                push_chunk(
                    &mut chunks,
                    path,
                    ChunkKind::ProducerMetadata,
                    heading_path.clone(),
                    body,
                );
            }
        }
    }

    chunks
}

/// Append a chunk, assigning its `ord` (and thus `chunk_id = sha256(path#ord)`) from the current
/// output position — so dropping an empty section renumbers everything after it.
fn push_chunk(
    chunks: &mut Vec<Chunk>,
    path: &str,
    kind: ChunkKind,
    heading_path: String,
    body: String,
) {
    let ord = chunks.len();
    chunks.push(Chunk {
        id: sha256_hex(format!("{path}#{ord}").as_bytes()),
        ord,
        kind,
        heading_path,
        body,
    });
}

/// Pack a section's segments into chunk bodies, each ≈ ≤ `CHUNK_BUDGET_TOKENS` (an oversize fenced
/// code block is the one allowed exception — kept atomic). May return a single empty body for a
/// heading-only section; the caller keeps a bodyless *leaf* heading as a heading-only chunk (so its
/// heading text stays searchable) and drops an empty section whose breadcrumb a descendant carries.
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
    let max_chars = ((budget as f32) * 3.5).floor().max(1.0) as usize;
    for word in para.split_whitespace() {
        if estimate_tokens(word) > budget {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            // JSON strings, URLs, and hashes can be one enormous whitespace-free token. Split such a
            // run by Unicode scalar count rather than silently exceeding the embedding window.
            let mut piece = String::new();
            let mut piece_chars = 0;
            for ch in word.chars() {
                piece.push(ch);
                piece_chars += 1;
                if piece_chars == max_chars {
                    out.push(std::mem::take(&mut piece));
                    piece_chars = 0;
                }
            }
            if !piece.is_empty() {
                out.push(piece);
            }
            continue;
        }
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
    fn vagus_lifecycle_frontmatter_is_not_indexed() {
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
        assert!(!all.contains("chat"), "source leaked into chunks: {all}");
        assert!(all.contains("Title"));
        assert!(all.contains("body text"));
    }

    #[test]
    fn producer_json_is_a_dedicated_searchable_chunk_after_content() {
        let md = concat!(
            "---\n",
            "created: 2026-05-29T18:02\n",
            "status: inbox\n",
            "corti: {\"schema\":1,\"mode\":\"live\",\"models\":{\"asr\":{\"id\":\"nvidia/parakeet-tdt-0.6b-v3\"}}}\n",
            "---\n\n# Transcript\n\nspoken words\n",
        );
        let chunks = chunk_markdown("p.md", md);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].kind, ChunkKind::Content);
        assert_eq!(chunks[0].id, sha256_hex(b"p.md#0"));
        assert_eq!(chunks[0].body, "spoken words");
        assert_eq!(chunks[1].kind, ChunkKind::ProducerMetadata);
        assert_eq!(chunks[1].id, sha256_hex(b"p.md#1"));
        assert_eq!(chunks[1].heading_path, "Frontmatter > corti");
        assert!(chunks[1].body.contains("parakeet-tdt-0.6b-v3"));
        assert!(chunks[1].body.contains("mode live"));
        assert!(!chunks[1].body.contains("created"));
        assert!(!chunks[1].body.contains("status"));
    }

    #[test]
    fn unclosed_frontmatter_does_not_create_a_producer_metadata_chunk() {
        let chunks = chunk_markdown(
            "broken.md",
            "---\ncorti: {\"models\":{\"asr\":\"parakeet\"}}\n# Body\nwords\n",
        );
        assert!(chunks.iter().all(|c| c.kind == ChunkKind::Content));
    }

    #[test]
    fn long_unbroken_producer_value_stays_inside_the_chunk_budget() {
        let value = "x".repeat(10_000);
        let md = format!("---\nproducer: {{\"value\":\"{value}\"}}\n---\n");
        let chunks = chunk_markdown("large.md", &md);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.kind == ChunkKind::ProducerMetadata));
        for chunk in chunks {
            assert!(
                estimate_tokens(&chunk.body) <= CHUNK_BUDGET_TOKENS + CHUNK_OVERLAP_TOKENS + 5,
                "metadata chunk over budget: {} tokens",
                estimate_tokens(&chunk.body)
            );
        }
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
    fn empty_leaf_section_keeps_its_heading_for_search() {
        // Middle H2 has no body but is a leaf (no descendant). Its heading tokens are only indexed
        // via its own heading_path, so it survives as a heading-only chunk between its neighbours.
        let md = "# T\n## A\nalpha\n## Empty\n## B\nbeta\n";
        let c = chunk_markdown("m.md", md);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].heading_path, "T > A");
        assert_eq!(c[1].heading_path, "T > Empty");
        assert_eq!(
            c[1].body, "Empty",
            "bodyless leaf keeps its heading as body"
        );
        assert_eq!(c[2].heading_path, "T > B");
        // ord/chunk_id pin: the empty preamble ("") and bare "T" ancestor were dropped, so "T > A"
        // is renumbered to ord 0 (not its raw section index). This is the shift the reindex exists for.
        assert_eq!(c[0].id, sha256_hex(b"m.md#0"));
        assert_eq!(c[1].id, sha256_hex(b"m.md#1"));
        assert_eq!(c[2].id, sha256_hex(b"m.md#2"));
    }

    #[test]
    fn outline_stub_keeps_every_leaf_heading_findable() {
        // A skeleton note (all sections empty): each leaf heading stays findable; the shared "Meeting"
        // ancestor survives in every leaf's breadcrumb, so no separate ancestor chunk is emitted.
        let md = "# Meeting\n## Agenda\n## Notes\n## Actions\n";
        let c = chunk_markdown("o.md", md);
        assert_eq!(
            c.iter()
                .map(|c| c.heading_path.as_str())
                .collect::<Vec<_>>(),
            vec!["Meeting > Agenda", "Meeting > Notes", "Meeting > Actions"]
        );
        assert_eq!(
            c.iter().map(|c| c.body.as_str()).collect::<Vec<_>>(),
            vec!["Agenda", "Notes", "Actions"]
        );
    }

    #[test]
    fn h1_title_with_prose_only_under_h2_has_no_empty_preamble() {
        let md = "# Title\n## Section\nthe actual prose\n";
        let c = chunk_markdown("t.md", md);
        assert_eq!(c.len(), 1, "the ord-0 title preamble must not be emitted");
        assert_eq!(c[0].heading_path, "Title > Section");
        assert!(c[0].body.contains("actual prose"));
        // The empty "" and "Title" ancestors were dropped (their tokens live in the breadcrumb), so
        // the surviving chunk is ord 0, not ord 2.
        assert_eq!(c[0].id, sha256_hex(b"t.md#0"));
    }

    #[test]
    fn bare_title_only_note_stays_findable_with_one_chunk() {
        let c = chunk_markdown("foo.md", "# Foo\n");
        assert_eq!(c.len(), 1, "a title-only stub must keep exactly one chunk");
        assert_eq!(c[0].heading_path, "Foo");
        // Fallback body is the leaf heading, not empty — a meaningful embedding, findable by title.
        assert_eq!(c[0].body, "Foo");
    }

    #[test]
    fn whitespace_only_body_is_treated_as_empty() {
        // The section under ## S is only whitespace; it is treated exactly like an empty leaf — kept
        // as a heading-only chunk whose body is the heading "S", never the whitespace.
        let md = "# T\n## S\n   \n## Real\ncontent\n";
        let c = chunk_markdown("w.md", md);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].heading_path, "T > S");
        assert_eq!(c[0].body, "S");
        assert_eq!(c[1].heading_path, "T > Real");
        assert!(c[1].body.contains("content"));
    }

    #[test]
    fn truly_empty_note_yields_no_chunks() {
        // No heading, no prose: nothing is searchable, so nothing is indexed (no garbage empty-body
        // vector). index.rs tolerates a zero-chunk note.
        assert!(chunk_markdown("e.md", "").is_empty());
        assert!(chunk_markdown("e.md", "   \n\n \t\n").is_empty());
    }

    #[test]
    fn no_emitted_chunk_has_empty_body_the_bodyless_leaf_keeps_its_heading() {
        // A skeleton with empty sections plus real ones: no chunk has an empty body, ords are dense,
        // and the bodyless leaf "## Two" survives with its heading as body.
        let md = "# Root\n## One\nfirst\n## Two\n## Three\nthird\n#### inline\n";
        let c = chunk_markdown("s.md", md);
        assert!(c.iter().all(|c| !c.body.trim().is_empty()));
        let two = c.iter().find(|c| c.heading_path == "Root > Two").unwrap();
        assert_eq!(two.body, "Two");
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
