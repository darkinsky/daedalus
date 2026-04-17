mod factory;
mod memory;
mod note;
mod prompts;
mod store;

pub use factory::AgenticFactory;
// Internal types — accessible via `super::` within the module, not re-exported.
// AgenticMemory, MemoryNote, AgenticMemoryStore are used only within this submodule.
