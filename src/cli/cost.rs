//! Session cost re-export for backward compatibility.
//!
//! `SessionCost` now lives in `middleware::builtin::cost` (its canonical home)
//! to avoid a reverse dependency from `agent` → `cli`.
//!
//! All internal code now imports directly from `middleware::builtin::cost`.
//! This module is kept as a placeholder; it can be removed in a future
//! cleanup pass once no external consumers depend on the old path.
