extern crate bf;
extern crate clap;

use std::io::{self, Read};
use std::fs::File;
use bf::*;
use clap::{Arg, App};

fn main() {
    let program = parser::parse_program(&get_program()).unwrap();
    let mut state = state::State::new();
    interpreter::interpret(&program, &mut state, &mut io::stdin(), &mut io::stdout());
}

fn get_program() -> Vec<u8> {
    let matches = App::new("bfi")
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
            .help("The source file to interpret")
            .multiple(true)
            .conflicts_with("expr")
            .index(1))
        .get_matches();

    let mut input = Vec::new();
    if let Some(exprs) = matches.values_of("expr") {
        for e in exprs {
            input.extend(e.as_bytes());
        }
    } else if let Some(files) = matches.values_of("INPUT") {
        for f in files {
            let mut file = File::open(f).unwrap();
            file.read_to_end(&mut input).unwrap();
        }
    }

    input
}
