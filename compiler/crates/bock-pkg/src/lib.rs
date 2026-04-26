//! Bock pkg — package manager for resolving and fetching Bock dependencies.
//!
//! This crate provides:
//! - Manifest (`bock.package`) parsing and manipulation
//! - Dependency resolution using the PubGrub algorithm
//! - Lockfile (`bock.lock`) generation and reading
//! - High-level commands for `bock pkg add/remove/tree`

pub mod commands;
pub mod error;
pub mod install;
pub mod lockfile;
pub mod manifest;
pub mod network;
pub mod resolver;
pub mod tree;
pub mod version;
