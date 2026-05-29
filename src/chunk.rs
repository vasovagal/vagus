//! Heading-aware Markdown chunking.
//!
//! Split a note on H1–H3 headings into sections; each section is one chunk carrying its
//! heading-path breadcrumb (e.g. "H1 > H2"). Code blocks are kept whole (we only ever split at
//! headings, and a fenced block contains no headings). H4–H6 headings stay inline as body text.
//! A note with no headings yields a single chunk (so bare `vim`-typed notes index fine — G3).
//!
//! `chunk_id = sha256(path + "#" + ord)` is stable for a stable file, so re-chunking an unchanged
//! file yields identical ids.

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

use crate::util::sha256_hex;

#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: String,
    pub ord: usize,
    pub heading_path: String,
    pub body: String,
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

/// Split `text` (the note at vault-relative `path`) into heading-aware chunks.
pub fn chunk_markdown(path: &str, text: &str) -> Vec<Chunk> {
    // Don't index YAML frontmatter (created/status/source/…) as note content.
    let md = strip_frontmatter(text);

    // Heading breadcrumb stack of (level, text) for levels 1..=3.
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut sections: Vec<(String, String)> = Vec::new(); // (heading_path, body)
    let mut body = String::new();
    let mut heading_buf = String::new();
    let mut in_heading: Option<usize> = None;

    let heading_path = |stack: &[(usize, String)]| -> String {
        stack
            .iter()
            .map(|(_, t)| t.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" > ")
    };

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
                    sections.push((heading_path(&stack), std::mem::take(&mut body)));
                    stack.retain(|(l, _)| *l < lvl);
                    stack.push((lvl, title));
                } else {
                    // H4–H6: keep the heading text inline in the body.
                    body.push_str(&title);
                    body.push('\n');
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if in_heading.is_some() {
                    heading_buf.push_str(&t);
                } else {
                    body.push_str(&t);
                }
            }
            Event::SoftBreak | Event::HardBreak | Event::Rule => body.push('\n'),
            Event::End(TagEnd::Paragraph) | Event::End(TagEnd::CodeBlock) => body.push('\n'),
            _ => {}
        }
    }
    sections.push((heading_path(&stack), body));

    let mut chunks = Vec::new();
    for (heading_path, body) in sections {
        if body.trim().is_empty() && heading_path.is_empty() {
            continue; // skip empty preamble
        }
        let ord = chunks.len();
        chunks.push(Chunk {
            id: sha256_hex(format!("{path}#{ord}").as_bytes()),
            ord,
            heading_path,
            body: body.trim().to_string(),
        });
    }
    chunks
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
}
