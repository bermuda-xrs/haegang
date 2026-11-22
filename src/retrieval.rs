//! Retrieval for "constant injection": pull the persona's own past replies that fit the current message.
//!
//! The corpus unit is an [`Exchange`]: a persona turn plus the turn it answered. [`Bm25Retriever`]
//! scores a query against the answered turn (strong) and the reply (weak) through inverted indexes,
//! then boosts recent exchanges a little. Other strategies implement [`Retriever`]; see `dense`.

use crate::{ChatMessage, Message, Persona};
use std::collections::HashMap;

/// A longer gap starts a new turn and breaks the link between one turn and the next.
const TURN_GAP_MS: u64 = 60 * 60 * 1000;
/// A turn stops growing here: pages of narration are many passages, not one reply.
const MAX_TURN_BYTES: usize = 1500;

/// A persona turn and what it answered. `context` is empty if nothing preceded it.
#[derive(Debug, Clone)]
pub struct Exchange {
    pub context: String,
    pub reply: String,
    pub timestamp_ms: i64,
}

/// A retrieved exchange with its relevance score.
#[derive(Debug, Clone)]
pub struct Exemplar {
    pub context: String,
    pub reply: String,
    pub timestamp_ms: i64,
    pub score: f32,
}

/// What retrievers are asked about: the incoming message plus the conversation so far.
pub struct Query<'a> {
    pub current: &'a str,
    pub history: &'a [ChatMessage],
}

impl Query<'_> {
    /// Bag-of-words text: the current message counts double, the last two turns add topic context.
    pub fn lexical(&self) -> String {
        let mut q = format!("{0}\n{0}", self.current);
        for m in self.history.iter().rev().take(2) {
            q.push('\n');
            q.push_str(&m.text);
        }
        q
    }
}

pub trait Retriever: Send + Sync {
    /// Top-`k` exemplars, best first. Irrelevant documents must be omitted.
    fn retrieve(&self, q: &Query, k: usize) -> Vec<Exemplar>;
}

#[derive(Debug, Clone)]
pub struct InjectionConfig {
    /// Most retrieved exchanges.
    pub k: usize,
    /// Number of always-included style samples.
    pub style_samples: usize,
}

impl Default for InjectionConfig {
    fn default() -> Self {
        Self { k: 6, style_samples: 8 }
    }
}

/// The block to inject into a prompt; see [`crate::prepare_chat`] and [`crate::prepare_prompt`].
#[derive(Debug, Clone, Default)]
pub struct InjectionContext {
    /// Query-relevant real exchanges.
    pub exemplars: Vec<Exemplar>,
    /// Real persona messages spread over the history, independent of the query, to steady the voice.
    pub style_samples: Vec<String>,
}

/// Retrieve with the persona's own retriever and the default config.
pub fn get_injection_context(
    persona: &Persona,
    current_message: &str,
    recent_history: &[ChatMessage],
) -> InjectionContext {
    let cfg = InjectionConfig::default();
    get_injection_context_with(persona, persona.retriever(), current_message, recent_history, &cfg)
}

pub fn get_injection_context_with(
    persona: &Persona,
    retriever: &dyn Retriever,
    current_message: &str,
    recent_history: &[ChatMessage],
    cfg: &InjectionConfig,
) -> InjectionContext {
    let q = Query { current: current_message, history: recent_history };
    InjectionContext {
        exemplars: retriever.retrieve(&q, cfg.k),
        style_samples: persona.style_samples(cfg.style_samples),
    }
}

/// Group messages into turns (same sender, short gaps) and pair each persona turn with the one before.
pub(crate) fn build_exchanges(messages: &[Message], target: &str) -> Vec<Exchange> {
    struct Turn<'a> {
        persona: bool,
        start_ms: i64,
        end_ms: i64,
        sender: &'a str,
        text: String,
    }

    let mut turns: Vec<Turn> = Vec::new();
    for m in messages {
        let grows = turns.last().is_some_and(|t| {
            t.sender == m.sender && m.timestamp_ms.abs_diff(t.end_ms) <= TURN_GAP_MS && t.text.len() < MAX_TURN_BYTES
        });
        match turns.last_mut() {
            Some(t) if grows => {
                t.end_ms = m.timestamp_ms;
                t.text.push('\n');
                t.text.push_str(&m.text);
            }
            _ => turns.push(Turn {
                persona: m.sender == target,
                start_ms: m.timestamp_ms,
                end_ms: m.timestamp_ms,
                sender: &m.sender,
                text: m.text.clone(),
            }),
        }
    }
    turns
        .iter()
        .enumerate()
        .filter(|(_, t)| t.persona)
        .map(|(i, t)| {
            let answered = i.checked_sub(1).map(|j| &turns[j]);
            let context = match answered {
                Some(p) if !p.persona && t.start_ms.abs_diff(p.end_ms) <= TURN_GAP_MS => p.text.clone(),
                _ => String::new(),
            };
            Exchange { context, reply: t.text.clone(), timestamp_ms: t.start_ms }
        })
        .collect()
}

/// Lowercased words. A non-ASCII word also yields its character bigrams, which is what makes
/// unsegmented CJK and agglutinative Korean searchable.
fn tokenize_into(s: &str, out: &mut Vec<String>) {
    out.clear();
    for w in s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        let w = w.to_lowercase();
        if !w.is_ascii() {
            let cs: Vec<char> = w.chars().collect();
            out.extend(cs.windows(2).map(|p| p.iter().collect::<String>()));
        }
        out.push(w);
    }
}

/// Keep the `k` highest-scoring entries, sorted best first. O(n + k log k).
pub(crate) fn top_k(v: &mut Vec<(usize, f32)>, k: usize) {
    let by_score = |a: &(usize, f32), b: &(usize, f32)| b.1.total_cmp(&a.1);
    if v.len() > k {
        v.select_nth_unstable_by(k, by_score);
        v.truncate(k);
    }
    v.sort_by(by_score);
}

/// One BM25 field behind an inverted index: query cost scales with the matching postings, not with
/// the size of the corpus.
struct Field {
    dict: HashMap<String, u32>,
    postings: Vec<Vec<(u32, u32)>>, // term id -> (document, term frequency)
    lens: Vec<f32>,
    avg_len: f32,
}

impl Field {
    fn build<'a>(docs: impl Iterator<Item = &'a str>) -> Self {
        let mut dict: HashMap<String, u32> = HashMap::new();
        let mut postings: Vec<Vec<(u32, u32)>> = Vec::new();
        let mut lens = Vec::new();
        let (mut tokens, mut ids) = (Vec::new(), Vec::new());
        for (d, text) in docs.enumerate() {
            tokenize_into(text, &mut tokens);
            ids.clear();
            for t in tokens.drain(..) {
                // one string allocation per *distinct* term, not per occurrence
                let id = match dict.get(&t) {
                    Some(&id) => id,
                    None => {
                        let id = postings.len() as u32;
                        postings.push(Vec::new());
                        dict.insert(t, id);
                        id
                    }
                };
                ids.push(id);
            }
            lens.push(ids.len() as f32);
            ids.sort_unstable();
            for run in ids.chunk_by(|a, b| a == b) {
                postings[run[0] as usize].push((d as u32, run.len() as u32));
            }
        }
        let avg_len = (lens.iter().sum::<f32>() / lens.len().max(1) as f32).max(1.0);
        Field { dict, postings, lens, avg_len }
    }

    fn accumulate(&self, query: &HashMap<String, u32>, scores: &mut [f32], weight: f32) {
        const K1: f32 = 1.2;
        const B: f32 = 0.75;
        let n = self.lens.len() as f32;
        for (term, &qn) in query {
            let Some(&id) = self.dict.get(term) else { continue };
            let posting = &self.postings[id as usize];
            let df = posting.len() as f32;
            let w = weight * (1.0 + (n - df + 0.5) / (df + 0.5)).ln() * (qn as f32).sqrt();
            for &(d, tf) in posting {
                let tf = tf as f32;
                let norm = 1.0 - B + B * self.lens[d as usize] / self.avg_len;
                scores[d as usize] += w * tf * (K1 + 1.0) / (tf + K1 * norm);
            }
        }
    }
}

/// Two-field BM25 (what was said to the persona, and their reply) with a recency boost.
pub struct Bm25Retriever {
    exchanges: Vec<Exchange>,
    context: Field,
    reply: Field,
    ts_range: (i64, i64),
}

impl Bm25Retriever {
    const REPLY_WEIGHT: f32 = 0.4;
    const RECENCY_BOOST: f32 = 0.25;
    /// Over-fetch before the recency boost, which can reorder the top of the list.
    const OVERFETCH: usize = 4;

    pub fn new(exchanges: Vec<Exchange>) -> Self {
        let context = Field::build(exchanges.iter().map(|e| e.context.as_str()));
        let reply = Field::build(exchanges.iter().map(|e| e.reply.as_str()));
        let min = exchanges.iter().map(|e| e.timestamp_ms).min().unwrap_or(0);
        let max = exchanges.iter().map(|e| e.timestamp_ms).max().unwrap_or(0);
        Self { exchanges, context, reply, ts_range: (min, max) }
    }

    pub fn exchanges(&self) -> &[Exchange] {
        &self.exchanges
    }

    /// Raw BM25 ranking of the matching exchanges: `(index, score)`, best first.
    pub(crate) fn rank(&self, text: &str, n: usize) -> Vec<(usize, f32)> {
        let mut tokens = Vec::new();
        tokenize_into(text, &mut tokens);
        let mut q: HashMap<String, u32> = HashMap::new();
        for t in tokens {
            *q.entry(t).or_insert(0) += 1;
        }
        let mut scores = vec![0f32; self.exchanges.len()];
        self.context.accumulate(&q, &mut scores, 1.0);
        self.reply.accumulate(&q, &mut scores, Self::REPLY_WEIGHT);
        let mut hits: Vec<(usize, f32)> = scores.into_iter().enumerate().filter(|&(_, s)| s > 0.0).collect();
        top_k(&mut hits, n);
        hits
    }

    /// Favour recent exchanges, then keep the best `k`.
    pub(crate) fn boost_recent(&self, hits: &mut Vec<(usize, f32)>, k: usize) {
        let span = self.ts_range.1.abs_diff(self.ts_range.0).max(1) as f32;
        for (i, score) in hits.iter_mut() {
            let age = self.exchanges[*i].timestamp_ms.abs_diff(self.ts_range.0) as f32 / span;
            *score *= 1.0 + Self::RECENCY_BOOST * age;
        }
        top_k(hits, k);
    }

    pub(crate) fn exemplar(&self, i: usize, score: f32) -> Exemplar {
        let e = &self.exchanges[i];
        Exemplar { context: e.context.clone(), reply: e.reply.clone(), timestamp_ms: e.timestamp_ms, score }
    }
}

impl Retriever for Bm25Retriever {
    fn retrieve(&self, q: &Query, k: usize) -> Vec<Exemplar> {
        let mut hits = self.rank(&q.lexical(), k.max(1) * Self::OVERFETCH);
        self.boost_recent(&mut hits, k);
        hits.into_iter().map(|(i, s)| self.exemplar(i, s)).collect()
    }
}
