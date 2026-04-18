pub(crate) mod config;
mod factory;
mod memory;
mod palace;
mod knowledge_graph;
mod graph;
mod diary;
mod identity;
mod classifier;
mod prompts;
mod retriever;
mod store;
mod wal;
mod normalize;
mod dedup;
mod entity_detector;
mod dialect;
mod query_sanitizer;
mod stopwords;

pub use factory::MemPalaceFactory;
// Internal types — accessible via `super::` within the module, not re-exported.
// MemPalaceConfig, MemPalaceMemory, HallType, Palace, MemPalaceStore are used only within this submodule.
