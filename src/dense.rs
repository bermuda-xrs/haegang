//! Embedding retrieval, and its fusion with BM25.
//!
//! Vectors are L2-normalised f32 in one contiguous buffer, so cosine similarity is a dot product in a
//! tight loop the compiler vectorises. The same buffer is what gets written next to the persona.

use crate::retrieval::{top_k, Bm25Retriever, Exchange, Exemplar, Query, Retriever};
use crate::{Embedder, Error, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

const MAGIC: &[u8; 4] = b"PCEM";
const VERSION: u32 = 1;
const HEADER_BYTES: usize = 20;
const BATCH: usize = 32;
const MAX_EMBED_CHARS: usize = 1000;
/// How many candidates each ranking contributes to the fusion.
const POOL: usize = 50;
/// Reciprocal-rank fusion damping; 60 is the value from the original paper.
const RRF_K: f32 = 60.0;

pub struct DenseIndex {
    pub model: String,
    dim: usize,
    data: Vec<f32>, // rows of `dim` floats
}

/// nomic-embed-text wants to be told whether it is embedding a document or a query.
fn prefixed(model: &str, text: &str, query: bool) -> String {
    let t: String = text.chars().take(MAX_EMBED_CHARS).collect();
    match model.starts_with("nomic-embed") {
        true => format!("{} {t}", if query { "search_query:" } else { "search_document:" }),
        false => t,
    }
}

fn normalise(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

/// What gets embedded for an exchange: what was said to the persona, else their own reply.
fn doc_text(e: &Exchange) -> &str {
    if e.context.is_empty() { &e.reply } else { &e.context }
}

impl DenseIndex {
    pub fn len(&self) -> usize {
        self.data.len().checked_div(self.dim).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn build(exchanges: &[Exchange], embedder: &dyn Embedder, model: &str) -> Result<Self> {
        let (mut dim, mut data) = (0usize, Vec::new());
        for batch in exchanges.chunks(BATCH) {
            let texts: Vec<String> = batch.iter().map(|e| prefixed(model, doc_text(e), false)).collect();
            for mut v in embedder.embed(&texts)? {
                if dim == 0 {
                    dim = v.len();
                    data.reserve(exchanges.len() * dim);
                }
                if v.len() != dim {
                    return Err(Error::Backend("inconsistent embedding dimensions".into()));
                }
                normalise(&mut v);
                data.extend_from_slice(&v);
            }
        }
        Ok(Self { model: model.to_string(), dim, data })
    }

    /// Top-`n` by cosine similarity: `(exchange index, similarity)`.
    pub fn search(&self, query: &[f32], n: usize) -> Vec<(usize, f32)> {
        if self.dim == 0 || query.len() != self.dim {
            return Vec::new();
        }
        let mut q = query.to_vec();
        normalise(&mut q);
        let mut hits: Vec<(usize, f32)> = self
            .data
            .chunks_exact(self.dim)
            .enumerate()
            .map(|(i, row)| (i, row.iter().zip(&q).map(|(a, b)| a * b).sum()))
            .collect();
        top_k(&mut hits, n);
        hits
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut b = Vec::with_capacity(HEADER_BYTES + self.model.len() + self.data.len() * 4);
        b.extend_from_slice(MAGIC);
        for n in [VERSION, self.dim as u32, self.len() as u32, self.model.len() as u32] {
            b.extend_from_slice(&n.to_le_bytes());
        }
        b.extend_from_slice(self.model.as_bytes());
        for f in &self.data {
            b.extend_from_slice(&f.to_le_bytes());
        }
        std::fs::write(path, b).map_err(|e| Error::Io(format!("{}: {e}", path.display())))
    }

    /// `None` if the file is missing or malformed; the caller then rebuilds.
    pub fn load(path: &Path) -> Option<Self> {
        let b = std::fs::read(path).ok()?;
        let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()) as usize);
        if b.get(..4)? != MAGIC || u32_at(4)? != VERSION as usize {
            return None;
        }
        let (dim, count, model_len) = (u32_at(8)?, u32_at(12)?, u32_at(16)?);
        let model = String::from_utf8(b.get(HEADER_BYTES..HEADER_BYTES + model_len)?.to_vec()).ok()?;
        let body = b.get(HEADER_BYTES + model_len..)?;
        if body.len() != dim.checked_mul(count)?.checked_mul(4)? {
            return None;
        }
        let data = body.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
        Some(Self { model, dim, data })
    }
}

/// BM25 and embeddings fused by reciprocal-rank fusion. If the embedder fails at query time this
/// degrades to BM25 alone.
pub struct HybridRetriever {
    bm25: Bm25Retriever,
    dense: DenseIndex,
    embedder: Arc<dyn Embedder>,
}

impl HybridRetriever {
    /// `dense` must have been built from the same exchanges as `bm25`, in the same order.
    pub fn new(bm25: Bm25Retriever, dense: DenseIndex, embedder: Arc<dyn Embedder>) -> Result<Self> {
        if dense.len() != bm25.exchanges().len() {
            return Err(Error::Invalid("dense index and exchanges differ in length".into()));
        }
        Ok(Self { bm25, dense, embedder })
    }

    fn semantic(&self, query: &str) -> Vec<(usize, f32)> {
        self.embedder
            .embed(&[prefixed(&self.dense.model, query, true)])
            .ok()
            .and_then(|mut v| v.pop())
            .map(|v| self.dense.search(&v, POOL))
            .unwrap_or_default()
    }
}

impl Retriever for HybridRetriever {
    fn retrieve(&self, q: &Query, k: usize) -> Vec<Exemplar> {
        let mut fused: HashMap<usize, f32> = HashMap::new();
        for list in [self.bm25.rank(&q.lexical(), POOL), self.semantic(q.current)] {
            for (rank, (i, _)) in list.into_iter().enumerate() {
                *fused.entry(i).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
            }
        }
        let mut hits: Vec<(usize, f32)> = fused.into_iter().collect();
        self.bm25.boost_recent(&mut hits, k);
        hits.into_iter().map(|(i, s)| self.bm25.exemplar(i, s)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_search() {
        let idx = DenseIndex { model: "m".into(), dim: 2, data: vec![1.0, 0.0, 0.0, 1.0] };
        let p = std::env::temp_dir().join(format!("pc_dense_{}.bin", std::process::id()));
        idx.save(&p).unwrap();
        let loaded = DenseIndex::load(&p).unwrap();
        assert_eq!((loaded.len(), loaded.model.as_str()), (2, "m"));
        assert_eq!(loaded.search(&[0.1, 0.9], 1)[0].0, 1);
        assert!(loaded.search(&[0.1, 0.9, 0.3], 1).is_empty()); // wrong dimension
        std::fs::write(&p, b"junk").unwrap();
        assert!(DenseIndex::load(&p).is_none());
        let _ = std::fs::remove_file(p);
    }
}
