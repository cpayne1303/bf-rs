//! The Brainfuck interpreter executable.
//!
//! ```
//! USAGE:
//!     bfi [FLAGS] [OPTIONS] [--] [FILE]...
//!
//! FLAGS:
//!         --ast          Interpret the unoptimized AST
//!         --byte         Compile AST to bytecode
//!     -h, --help         Prints help information
//!         --jit          JIT to native x64 (default)
//!         --llvm         JIT using LLVM
//!         --peep         Interpret the peephole-optimized AST
//!         --rle          Interpret the run-length encoded the AST
//!     -u, --unchecked    Omit memory bounds checks in JIT
//!     -V, --version      Prints version information
//!
//! OPTIONS:
//!     -e, --expr <CODE>...    BF code to execute
//!     -s, --size <SIZE>       Memory size in bytes (default 30,000)
//!
//! ARGS:
//!     <FILE>...    The source file(s) to interpret
//! ```
//!
//! See [the library crate documentation](../bf/index.html) for more.
extern crate bf;
extern crate clap;
use bf::ast;
use bf::traits::*;
use clap::{Parser, ValueEnum};
use std::fs::File;
use std::io::Read;
use std::process::exit;

#[derive(Debug, Clone, Parser)]
#[clap(version = env!("CARGO_PKG_VERSION"))]
#[clap(author = "Jesse A. Tov <jesse.tov@gmail.com>")]
#[clap(name = "bfi")]
#[clap(about = "A brainfuck interpreter")]

struct NewOptions {
    #[clap(
        help = "The source file(s) to interpret",
        conflicts_with = "expressions"
    )]
    files: Option<Vec<String>>,
    #[clap(
        short = 'e',
        long = "expr",
        help = "BF code to execute",
        conflicts_with = "files"
    )]
    expressions: Option<Vec<String>>,
    #[clap(
        short = 's',
        long = "size",
        default_value_t = 30000,
        help = "Memory size in bytes (default 30,000)"
    )]
    memory_size: usize,
    #[clap(short = 'u', long="unchecked", help = "Omit memory bounds checks in JIT", conflicts_with_all = &["ast", "rle", "bytecode", "peephole", "llvm"], requires = "jit")]
    unchecked: bool,
    #[clap(long = "ast", help = "Interpret the unoptimized AST", conflicts_with_all=&["rle", "bytecode", "unchecked", "peephole", "jit", "llvm"])]
    ast: bool,
    #[clap(long="rle", help = "Interpret the run-length encoded the AST", conflicts_with_all=&["ast", "bytecode", "peephole", "jit", "llvm", "unchecked"])]
    rle: bool,
    #[clap(long = "byte", help = "Compile AST to bytecode", conflicts_with_all=&["ast", "rle", "jit", "llvm", "unchecked"])]
    bytecode: bool,
    #[clap(long="peep", default_value_t = true, help = "Interpret the peephole-optimized AST", conflicts_with_all = &["ast", "rle", "bytecode", "jit", "llvm", "unchecked"])]
    peephole: bool,
    #[cfg(feature = "jit")]
    #[clap(long = "jit", help = "JIT to native x64", conflicts_with_all = &["ast", "rle", "bytecode", "peephole", "llvm"])]
    jit: bool,
    #[cfg(feature = "llvm")]
    #[clap(long = "llvm", help = "JIT using LLVM", conflicts_with_all = &["ast", "rle", "bytecode", "peephole", "jit", "unchecked"])]
    llvm: bool,
}
#[derive(Debug, Clone)]
struct Options {
    program_text: Vec<u8>,
    memory_size: Option<usize>,
    compiler_pass: Pass,
    unchecked: bool,
}
impl Options {
    fn new(options: &NewOptions) -> Options {
        let compiler_pass = Pass::new(options);
        Options {
            program_text: Vec::new(),
            memory_size: Some(options.memory_size),
            compiler_pass,
            unchecked: options.unchecked,
        }
    }
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Pass {
    #[clap(name = "--ast")]
    Ast,
    #[clap(name = "--rle")]
    Rle,
    #[clap(name = "--byte")]
    Bytecode,
    #[clap(name = "--peep")]
    Peephole,
    #[cfg(feature = "jit")]
    #[clap(name = "--jit")]
    Jit,
    #[cfg(feature = "llvm")]
    #[clap(name = "llvm")]
    Llvm,
}
impl Pass {
    fn new(options: &NewOptions) -> Pass {
        if options.ast {
            return Pass::Ast;
        }
        if options.rle {
            return Pass::Rle;
        }
        if options.bytecode {
            return Pass::Bytecode;
        }
        if options.peephole {
            return Pass::Peephole;
        }
        #[cfg(feature = "jit")]
        if options.jit {
            return Pass::Jit;
        }
        #[cfg(feature = "llvm")]
        if options.llvm {
            return Pass::Llvm;
        }
        Pass::Peephole
    }
}
fn main() {
    let result = NewOptions::parse();
    if result.memory_size == 0 {
        error_exit(1, "error: memory size must be at least 1.");
    }
    let mut options = Options::new(&result);
    if let Option::Some(exprs) = result.expressions {
        for e in exprs {
            options.program_text.extend(e.as_bytes());
        }
    } else if let Option::Some(files) = result.files {
        for f in files {
            let mut file =
                File::open(f.clone()).unwrap_or_else(|e| error_exit(1, &format!("{}: {}", e, f)));
            file.read_to_end(&mut options.program_text)
                .unwrap_or_else(|e| error_exit(1, &format!("{}: {}", e, f)));
        }
    } else {
        error_exit(1, "error: no program given.");
    }
    let program = parse(&options);
    match options.compiler_pass {
        Pass::Ast => {
            interpret(&*program, &options);
        }

        Pass::Rle => {
            let program = program.rle_compile();
            interpret(&*program, &options);
        }

        Pass::Peephole => {
            if !options.unchecked {
                let program = program.peephole_compile();
                interpret(&*program, &options);
            } else {
                error_exit(
                    2,
                    "unchecked can not be used with the default pass (peephole)",
                );
            }
        }
        Pass::Bytecode => {
            let program = program.bytecode_compile();
            interpret(&*program, &options);
        }

        #[cfg(feature = "jit")]
        Pass::Jit => {
            let program = program.jit_compile(!options.unchecked);
            interpret(&program, &options);
        }

        #[cfg(feature = "llvm")]
        Pass::Llvm => {
            program
                .llvm_run(options.memory_size)
                .unwrap_or_else(|e| error_exit(3, &format!("runtime error: {}.", e)));
        }
    }
}

fn parse(options: &Options) -> Box<ast::Program> {
    ast::parse_program(&options.program_text)
        .unwrap_or_else(|e| error_exit(2, &format!("syntax error: {}.", e)))
}

fn interpret<P: Interpretable + ?Sized>(program: &P, options: &Options) {
    program
        .interpret_stdin(options.memory_size)
        .unwrap_or_else(|e| error_exit(3, &format!("runtime error: {}.", e)))
}

fn error_exit(code: i32, msg: &str) -> ! {
    eprintln!("bfi: {}", msg);
    exit(code)
}
