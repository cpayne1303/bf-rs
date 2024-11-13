use std::io::{Read, Write};

use super::*;
use crate::state::State;
use crate::traits::Interpretable;
use common::BfResult;

impl Interpretable for Program {
    fn interpret_state<R: Read, W: Write>(
        &self,
        mut state: State,
        mut input: R,
        mut output: W,
    ) -> BfResult<()> {
        interpret(self, &mut state, &mut input, &mut output)
    }
}

fn interpret<R, W>(
    instructions: &[Statement],
    state: &mut State,
    input: &mut R,
    output: &mut W,
) -> BfResult<()>
where
    R: Read,
    W: Write,
{
    for instruction in instructions {
        interpret_instruction(instruction, state, input, output)?;
    }

    Ok(())
}

fn interpret_instruction<R, W>(
    instructions: &Statement,
    state: &mut State,
    input: &mut R,
    output: &mut W,
) -> BfResult<()>
where
    R: Read,
    W: Write,
{
    use super::Statement::*;
    use common::Instruction::*;

    match *instructions {
        Instr(Left(count)) => state.left(count)?,

        Instr(Right(count)) => state.right(count)?,

        Instr(Add(amount)) => state.up(amount),

        Instr(In) => state.read(input),

        Instr(Out) => state.write(output),

        Instr(SetZero) => state.store(0),

        Instr(OffsetAddRight(offset)) => {
            let value = state.load();
            if value != 0 {
                state.store(0);
                state.up_pos_offset(offset, value)?;
            }
        }

        Instr(OffsetAddLeft(offset)) => {
            let value = state.load();
            if value != 0 {
                state.store(0);
                state.up_neg_offset(offset, value)?;
            }
        }

        Instr(FindZeroRight(skip)) => {
            while state.load() != 0 {
                state.right(skip)?;
            }
        }

        Instr(FindZeroLeft(skip)) => {
            while state.load() != 0 {
                state.left(skip)?;
            }
        }

        Instr(JumpZero(_)) | Instr(JumpNotZero(_)) => panic!("unexpected jump instruction"),

        Loop(ref body) => {
            while state.load() != 0 {
                interpret(body, state, input, output)?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::*;

    #[test]
    fn hello_world() {
        assert_parse_interpret(HELLO_WORLD_SRC, "", "Hello, World!");
    }

    #[test]
    fn factoring() {
        assert_parse_interpret(FACTOR_SRC, "2\n", "2: 2\n");
        assert_parse_interpret(FACTOR_SRC, "3\n", "3: 3\n");
        assert_parse_interpret(FACTOR_SRC, "6\n", "6: 2 3\n");
        assert_parse_interpret(FACTOR_SRC, "100\n", "100: 2 2 5 5\n");
    }

    fn assert_parse_interpret(program: &[u8], input: &str, output: &str) {
        let program = crate::ast::parse_program(program).unwrap();
        let program = crate::rle::compile(&program);
        let program = crate::peephole::compile(&program);
        assert_interpret(&*program, input.as_bytes(), output.as_bytes());
    }
}
