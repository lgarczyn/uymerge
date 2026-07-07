//! uymerge: structural 3-way merge for Unity YAML.
//!
//! Behavior is specified in docs/SPEC.md.
//! The Python file unityyamlmerge_fix.py in the repo root is the executable
//! reference for the codec, parsers, and verification rules.
//! Port, don't invent.

#![forbid(unsafe_code)]

pub mod cli;
pub mod codec;
pub mod diff3;
pub mod merge;
pub mod model;
pub mod verify;
