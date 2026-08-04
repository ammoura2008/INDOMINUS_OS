#![no_std]

// Placeholder library for the workspace-root package workaround.
// This crate is intentionally minimal so Cargo can treat the workspace as a
// package-root workspace without requiring a std-enabled target.

/// Placeholder symbol to keep the crate valid for Cargo metadata and checks.
pub fn placeholder() {}
