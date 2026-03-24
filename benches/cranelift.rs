#![feature(test)]
extern crate bf;
extern crate test;

#[cfg(feature = "cranelift")]
mod cranelift_only {
    use bf::ast;
    use bf::test_helpers;
    use bf::traits::{CraneliftCompilable, Interpretable};
    use test::Bencher;

    #[bench]
    fn compile_factor(b: &mut Bencher) {
        let program = ast::parse_program(test_helpers::FACTOR_SRC).unwrap();
        b.iter(|| program.cranelift_compile());
    }

    #[bench]
    fn run_factor_million(b: &mut Bencher) {
        let program = ast::parse_program(test_helpers::FACTOR_SRC).unwrap();
        let program = program.cranelift_compile();
        b.iter(|| program.interpret_memory(None, b"1000000\n").unwrap());
    }
}
