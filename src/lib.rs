//! Any document -> its characters -> personas -> retrieval-based "constant injection" -> a local model.
//!
//! [`ingest`] reads the files, [`identify`] works out who is in them, the user picks characters, and
//! [`PersonaStore::create_persona`] stores one persona each. [`Engine::reply`] then answers in character;
//! to drive a model yourself, use [`get_injection_context`] with [`prepare_chat`] or [`prepare_prompt`].

mod backend;
mod chat;
mod dense;
mod error;
mod http;
mod identify;
mod ingest;
mod persona;
mod prompt;
mod retrieval;
mod settings;
mod types;

pub use backend::{
    ChatBackend, ChatTurn, Embedder, GenParams, ModelInfo, OllamaBackend, OllamaEmbedder, OpenAiBackend, TurnRole,
};
pub use chat::{Engine, Reply};
pub use dense::{DenseIndex, HybridRetriever};
pub use error::{Error, Result};
pub use identify::{identify, Character, Identification, IdentifyOptions, ModelAccess, Progress};
pub use ingest::{ingest, ingest_files, Content, Corpus, Document, ParseOptions, Unreadable};
pub use persona::{Persona, PersonaMeta, PersonaStore};
pub use prompt::{prepare_chat, prepare_prompt, ChatMessage, Role};
pub use retrieval::{
    get_injection_context, get_injection_context_with, Bm25Retriever, Exchange, Exemplar, InjectionConfig,
    InjectionContext, Query, Retriever,
};
pub use settings::{resolve_model, BackendKind, Settings};
pub use types::{Conversation, Message};
