//! Full-text (BM25) index via tantivy.
//!
//! tantivy has no in-place update: per changed file we `delete_term` on the exact `path` term, then
//! re-`add_document` the file's chunks, then a single `commit()` (guardrail G6). The index is keyed
//! by the same vault-relative `path` and `chunk_id` as the SQLite store so the two stay consistent
//! off one hash-diff (G5).

use std::path::Path;

use anyhow::{Context, Result};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::{DocAddress, Index, IndexWriter, TantivyDocument, Term, doc};

use crate::chunk::Chunk;

const WRITER_HEAP: usize = 50_000_000;

pub struct Lex {
    index: Index,
    path: Field,
    chunk_id: Field,
    heading: Field,
    body: Field,
}

fn schema() -> (Schema, Field, Field, Field, Field) {
    let mut b = Schema::builder();
    // `path`: exact single-token string, indexed so we can delete-by-term.
    let path = b.add_text_field("path", STRING);
    // `chunk_id`: stored so search can retrieve it and join back to SQLite.
    let chunk_id = b.add_text_field("chunk_id", STRING | STORED);
    let text_opts = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("en_stem")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let heading = b.add_text_field("heading", text_opts.clone());
    let body = b.add_text_field("body", text_opts);
    let schema = b.build();
    (schema, path, chunk_id, heading, body)
}

impl Lex {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let (schema, path, chunk_id, heading, body) = schema();
        let mmap = tantivy::directory::MmapDirectory::open(dir)
            .with_context(|| format!("opening tantivy dir {}", dir.display()))?;
        let index = Index::open_or_create(mmap, schema).context("open_or_create tantivy index")?;
        Ok(Self {
            index,
            path,
            chunk_id,
            heading,
            body,
        })
    }

    pub fn writer(&self) -> Result<IndexWriter> {
        Ok(self.index.writer(WRITER_HEAP)?)
    }

    /// Delete a file's existing docs (by exact path term).
    pub fn delete_file(&self, writer: &IndexWriter, path: &str) {
        writer.delete_term(Term::from_field_text(self.path, path));
    }

    /// Delete-then-add a file's chunks. Caller commits once after a batch.
    pub fn replace_file(&self, writer: &IndexWriter, path: &str, chunks: &[Chunk]) -> Result<()> {
        writer.delete_term(Term::from_field_text(self.path, path));
        for c in chunks {
            writer.add_document(doc!(
                self.path => path,
                self.chunk_id => c.id.as_str(),
                self.heading => c.heading_path.as_str(),
                self.body => c.body.as_str(),
            ))?;
        }
        Ok(())
    }

    /// BM25 search over body + heading. Returns (chunk_id, bm25_score) in rank order (best first).
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<(String, f32)>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        // OR-by-default (QueryParser default): better recall; BM25 ranks, and RRF fuses with the
        // semantic side. The fallback below only handles parse errors from query punctuation.
        let parser = QueryParser::for_index(&self.index, vec![self.body, self.heading]);

        let parsed = match parser.parse_query(query) {
            Ok(q) => q,
            Err(_) => {
                // Sanitize: keep alphanumerics + spaces, OR the terms.
                let safe: String = query
                    .chars()
                    .map(|c| if c.is_alphanumeric() { c } else { ' ' })
                    .collect();
                let or_parser = QueryParser::for_index(&self.index, vec![self.body, self.heading]);
                match or_parser.parse_query(safe.trim()) {
                    Ok(q) => q,
                    Err(_) => return Ok(vec![]),
                }
            }
        };

        // 0.26: plain TopDocs is not a Collector; pick an ordering (relevance).
        let hits: Vec<(f32, DocAddress)> = searcher.search(
            &*parsed,
            &TopDocs::with_limit(limit.max(1)).order_by_score(),
        )?;
        let mut out = Vec::with_capacity(hits.len());
        for (score, addr) in hits {
            let doc: TantivyDocument = searcher.doc(addr)?;
            if let Some(id) = doc.get_first(self.chunk_id).and_then(|v| v.as_str()) {
                out.push((id.to_string(), score));
            }
        }
        Ok(out)
    }

    /// Segment-level stats from the tantivy index. A high segment count = fragmentation (per-file
    /// commits create segments until tantivy's merge policy consolidates them).
    pub fn segment_stats(&self) -> Result<SegmentStats> {
        let metas = self.index.searchable_segment_metas()?;
        Ok(SegmentStats {
            segments: metas.len(),
            docs: metas.iter().map(|m| m.num_docs()).sum(),
            deleted: metas.iter().map(|m| m.num_deleted_docs()).sum(),
        })
    }

    /// Force-merge all segments into one and garbage-collect the old files — physically dropping
    /// tombstoned docs. Cheap vs `reindex`: it only rewrites the inverted index, not the embeddings.
    pub fn compact(&self) -> Result<()> {
        let ids = self.index.searchable_segment_ids()?;
        let mut writer: IndexWriter = self.index.writer(WRITER_HEAP)?;
        if ids.len() > 1 {
            writer.merge(&ids).wait()?;
        }
        writer.garbage_collect_files().wait()?;
        writer.wait_merging_threads()?;
        Ok(())
    }
}

/// Tantivy segment statistics (segment count indicates fragmentation).
#[derive(Debug, Default)]
pub struct SegmentStats {
    pub segments: usize,
    pub docs: u32,
    pub deleted: u32,
}
