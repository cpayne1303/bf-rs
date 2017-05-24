extern crate bf;
extern crate clap;

use std::io::{self, Read, Write};
use std::fs::File;
use std::process::exit;

use clap::{Arg, App};

use bf::ast;
use bf::state;
use bf::rle_ast;
use bf::flat;
use bf::interpreter::Interpretable;

#[derive(Debug, Clone)]
struct Options {
    program_text: Vec<u8>,
    memory_size:  Option<usize>,
}

fn main() {
    // Process command-line options:
    let options = get_options();

    // Parse the program to AST:
    let program = parse(&options);

    // Two compilation/optimization passes:
    let program = rle_ast::compiler::compile(&program);
    let program = flat::compiler::compile(&program);

    // Run it:
    interpret(&*program, &options);
}

fn parse(options: &Options) -> ast::Program {
    ast::parser::parse_program(&options.program_text)
        .unwrap_or_else(|e| error_exit(2, format!("Syntax error: {:?}.", e)))
}

fn interpret<P: Interpretable + ?Sized>(program: &P, options: &Options) {
    let state = options.memory_size.map(state::State::with_capacity);
    program.interpret_stdin(state)
        .unwrap_or_else(|e| error_exit(3, format!("Runtime error: {:?}.", e)))
}

fn get_options() -> Options {
    let mut result = Options {
        program_text: Vec::new(),
        memory_size:  None,
    };

    let matches = build_clap_app().get_matches();

    if let Some(size) = matches.value_of("size") {
        let size = size.parse()
            .unwrap_or_else(|e| error_exit(1, format!("Could not parse memory size: {}", e)));
        result.memory_size = Some(size);
    }

    if let Some(exprs) = matches.values_of("expr") {
        for e in exprs {
            result.program_text.extend(e.as_bytes());
        }
    } else if let Some(files) = matches.values_of("INPUT") {
        for f in files {
            let mut file = File::open(f)
                .unwrap_or_else(|e| error_exit(1, format!("{}: {}", e, f)));
            file.read_to_end(&mut result.program_text)
                .unwrap_or_else(|e| error_exit(1, format!("{}: {}", e, f)));
        }
    } else {
        error_exit(1, "No program given.".to_owned());
    }

    result
}

fn build_clap_app() -> App<'static, 'static> {
    App::new("bfi")
        .author("Jesse A. Tov <jesse.tov@gmail.com>")
        .about("A Brainfuck interpreter")
        .arg(Arg::with_name("expr")
            .short("e")
            .long("expr")
            .value_name("CODE")
            .help("BF code to execute")
            .multiple(true)
            .takes_value(true)
            .conflicts_with("INPUT"))
        .arg(Arg::with_name("INPUT")
            .help("The source file(s) to interpret")
            .multiple(true)
            .conflicts_with("expr")
            .index(1))
        .arg(Arg::with_name("size")
            .short("s")
            .long("size")
            .help("Memory size in bytes")
            .takes_value(true))
}

fn error_exit(code: i32, msg: String) -> ! {
    writeln!(io::stderr(), "{}", msg).unwrap();
    exit(code)
}

