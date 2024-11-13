//! JIT compiler for Brainfuck based on LLVM.
//!
//! Enabled with `--features=llvm`. This is actually quite slow, because LLVM takes a long time
//! optimizing. However, the actual running of the optimized code appears to be quite fast.

mod compiler;
mod wrapper;

pub use self::compiler::{compile_and_run, LlvmCompilable};
