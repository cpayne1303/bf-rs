use dynasm::dynasm;
use dynasmrt::x64::Assembler;
use dynasmrt::{DynasmApi, DynasmLabelApi};

use super::analysis::{AbstractInterpreter, BoundsAnalysis, NoAnalysis};
use super::*;
use crate::common::Count;
use crate::peephole;
use rts;

/// Program forms that can be JIT compiled.
pub trait JitCompilable {
    /// Compile the given program into the peephole AST to prepare for JIT compilation.
    fn with_peephole<F, R>(&self, k: F) -> R
    where
        F: FnOnce(&peephole::Program) -> R;

    /// JIT compile the given program.
    fn jit_compile(&self, checked: bool) -> Program {
        self.with_peephole(|ast| compile(ast, checked))
    }
}

dynasm!(asm
    ; .alias pointer, r12
    ; .alias mem_start, r13
    ; .alias mem_limit, r14
    ; .alias rts, r15
);

/// Compiles peephole-optimized AST to x64 machine code.
///
/// Uses the `dynasmrt` assembler
pub fn compile(program: &peephole::Program, checked: bool) -> Program {
    if checked {
        let mut compiler = Compiler::<AbstractInterpreter>::new(program, true);
        compiler.compile(program);
        compiler.into_program()
    } else {
        let mut compiler = Compiler::<NoAnalysis>::new(program, false);
        compiler.compile(program);
        compiler.into_program()
    }
}

/// The compiler state.
struct Compiler<B: BoundsAnalysis> {
    /// The underlying assembler.
    asm: Assembler,
    /// The offset of the starting instruction for the object function.
    start: dynasmrt::AssemblyOffset,
    /// Whether we are emitting bounds checks.
    checked: bool,
    /// Abstract interpreter for bounds checking analysis.
    interpreter: B,
}

impl<B: BoundsAnalysis> Compiler<B> {
    fn new(program: &peephole::Program, checked: bool) -> Self {
        let asm = Assembler::new().expect("Could not create assembler");
        let start = asm.offset();

        let mut result = Compiler {
            asm: asm,
            start: start,
            checked: checked,
            interpreter: B::new(program),
        };

        result.emit_prologue();

        result
    }

    fn into_program(mut self) -> Program {
        self.emit_epilogue();

        Program {
            code: self.asm.finalize().unwrap(),
            start: self.start,
        }
    }

    fn emit_prologue(&mut self) {
        dynasm!(self.asm
        ; .alias pointer, r12
        ; .alias mem_start, r13
        ; .alias mem_limit, r14
        ; .alias rts, r15
                ; push r12
                ; push r13
                ; push r14
                ; push r15
                ; mov pointer, rcx      // first argument
                ; mov mem_start, rcx
                ; mov mem_limit, rcx
                ; add mem_limit, rdx    // second argument
                ; mov rts, r8           // third argument
            );
    }

    fn emit_epilogue(&mut self) {
        dynasm!(self.asm
        ; .alias pointer, r12
        ; .alias mem_start, r13
        ; .alias mem_limit, r14
        ; .alias rts, r15
                ; mov rax, rts::OKAY as i32
                ; jmp ->finish

                ; ->underflow:
                ; mov rax, rts::UNDERFLOW as i32
                ; jmp ->finish

                ; ->overflow:
                ; mov rax, rts::OVERFLOW as i32

                ; ->finish:
                ; pop r15
                ; pop r14
                ; pop r13
                ; pop r12
                ; ret
            );
    }

    fn compile(&mut self, program: &[peephole::Statement]) {
        for stm in program {
            self.compile_statement(stm);
        }
    }

    fn compile_statement(&mut self, stm: &peephole::Statement) {
        use crate::common::Instruction::*;
        use peephole::Statement::*;

        match *stm {
            Instr(Right(count)) => {
                let proved = self.interpreter.move_right(count);

                dynasm!(self.asm
                ; .alias pointer, r12
                ; .alias mem_start, r13
                ; .alias mem_limit, r14
                ; .alias rts, r15
                                ;; self.load_pos_offset(count, proved)
                                ; add pointer, rax
                            );
            }

            Instr(Left(count)) => {
                let proved = self.interpreter.move_left(count);

                dynasm!(self.asm
                ; .alias pointer, r12
                ; .alias mem_start, r13
                ; .alias mem_limit, r14
                ; .alias rts, r15
                                ;; self.load_neg_offset(count, proved)
                                ; sub pointer, rax
                            );
            }

            Instr(Add(count)) => {
                dynasm!(self.asm
                ; .alias pointer, r12
                ; .alias mem_start, r13
                ; .alias mem_limit, r14
                ; .alias rts, r15
                                ; add [pointer], BYTE count as i8
                            );
            }

            Instr(In) => {
                dynasm!(self.asm
                ; .alias pointer, r12
                ; .alias mem_start, r13
                ; .alias mem_limit, r14
                ; .alias rts, r15
                                ;; self.rts_call(rts::RtsState::read as _)
                                ; mov [pointer], al
                            );
            }

            Instr(Out) => {
                dynasm!(self.asm
                ; .alias pointer, r12
                ; .alias mem_start, r13
                ; .alias mem_limit, r14
                ; .alias rts, r15
                                ; xor rdx, rdx
                                ; mov dl, [pointer]
                                ;; self.rts_call(rts::RtsState::write as _)
                            );
            }

            Instr(SetZero) => {
                dynasm!(self.asm
                ; .alias pointer, r12
                ; .alias mem_start, r13
                ; .alias mem_limit, r14
                ; .alias rts, r15
                                ; mov BYTE [pointer], 0
                            )
            }

            Instr(FindZeroRight(skip)) => {
                self.interpreter.reset_right();

                dynasm!(self.asm
                ; .alias pointer, r12
                ; .alias mem_start, r13
                ; .alias mem_limit, r14
                ; .alias rts, r15
                                ; jmp >end_loop
                                ; begin_loop:
                                ;; self.load_pos_offset(skip, false)
                                ; add pointer, rax
                                ; end_loop:
                                ; cmp BYTE [pointer], 0
                                ; jnz <begin_loop
                            )
            }

            Instr(FindZeroLeft(skip)) => {
                self.interpreter.reset_left();

                dynasm!(self.asm
                ; .alias pointer, r12
                ; .alias mem_start, r13
                ; .alias mem_limit, r14
                ; .alias rts, r15
                                ; jmp >end_loop
                                ; begin_loop:
                                ;; self.load_neg_offset(skip, false)
                                ; sub pointer, rax
                                ; end_loop:
                                ; cmp BYTE [pointer], 0
                                ; jnz <begin_loop
                            )
            }

            Instr(OffsetAddRight(offset)) => {
                let proved = self.interpreter.check_right(offset);

                dynasm!(self.asm
                ; .alias pointer, r12
                ; .alias mem_start, r13
                ; .alias mem_limit, r14
                ; .alias rts, r15
                                ; cmp BYTE [pointer], 0
                                ; jz >skip
                                ;; self.load_pos_offset(offset, proved)
                                ; mov cl, BYTE [pointer]
                                ; mov BYTE [pointer], 0
                                ; add BYTE [pointer + rax], cl
                                ; skip:
                            );
            }

            Instr(OffsetAddLeft(offset)) => {
                let proved = self.interpreter.check_left(offset);

                dynasm!(self.asm
                ; .alias pointer, r12
                ; .alias mem_start, r13
                ; .alias mem_limit, r14
                ; .alias rts, r15
                                ; cmp BYTE [pointer], 0
                                ; jz >skip
                                ;; self.load_neg_offset(offset, proved)
                                ; mov cl, BYTE [pointer]
                                ; mov BYTE [pointer], 0
                                ; neg rax
                                ; add BYTE [pointer + rax], cl
                                ; skip:
                            );
            }

            Instr(JumpZero(_)) | Instr(JumpNotZero(_)) => panic!("unexpected jump instruction"),

            Loop(ref body) => {
                let begin_label = self.asm.new_dynamic_label();
                let end_label = self.asm.new_dynamic_label();

                self.interpreter.enter_loop(body);

                dynasm!(self.asm
                ; .alias pointer, r12
                ; .alias mem_start, r13
                ; .alias mem_limit, r14
                ; .alias rts, r15
                                ; jmp =>end_label
                                ; =>begin_label
                                ;; self.compile(body)
                                ; =>end_label
                                ; cmp BYTE [pointer], 0
                                ; jnz =>begin_label
                            );

                self.interpreter.leave_loop();
            }
        }
    }

    fn rts_call(&mut self, fun: i64) {
        dynasm!(self.asm
        ; .alias pointer, r12
        ; .alias mem_start, r13
        ; .alias mem_limit, r14
        ; .alias rts, r15
                ; mov rax, QWORD fun
                ; mov rcx, rts
                ; sub rsp, BYTE 0x28
                ; call rax
                ; add rsp, BYTE 0x28
            );
    }

    #[inline]
    fn load_constant(&mut self, count: Count) {
        if count as i32 as Count == count {
            dynasm!(self.asm
            ; .alias pointer, r12
            ; .alias mem_start, r13
            ; .alias mem_limit, r14
            ; .alias rts, r15
                        ; mov rax, DWORD count as i32
                    );
        } else {
            dynasm!(self.asm
            ; .alias pointer, r12
            ; .alias mem_start, r13
            ; .alias mem_limit, r14
            ; .alias rts, r15
                        ; mov rax, QWORD count as i64
                    );
        }
    }

    #[inline]
    fn load_pos_offset(&mut self, offset: Count, proved: bool) {
        self.load_constant(offset);

        if self.checked && !proved {
            dynasm!(self.asm
            ; .alias pointer, r12
            ; .alias mem_start, r13
            ; .alias mem_limit, r14
            ; .alias rts, r15
                        ; mov rcx, mem_limit
                        ; sub rcx, pointer
                        ; cmp rcx, rax
                        ; jle ->overflow
                    );
        }
    }

    #[inline]
    fn load_neg_offset(&mut self, offset: Count, proved: bool) {
        self.load_constant(offset);

        if self.checked && !proved {
            dynasm!(self.asm
            ; .alias pointer, r12
            ; .alias mem_start, r13
            ; .alias mem_limit, r14
            ; .alias rts, r15
                        ; mov rcx, pointer
                        ; sub rcx, mem_start
                        ; cmp rcx, rax
                        ; jl ->underflow
                    );
        }
    }
}

impl JitCompilable for peephole::Program {
    fn with_peephole<F, R>(&self, k: F) -> R
    where
        F: FnOnce(&peephole::Program) -> R,
    {
        k(self)
    }
}

impl<T: peephole::PeepholeCompilable + ?Sized> JitCompilable for T {
    fn with_peephole<F, R>(&self, k: F) -> R
    where
        F: FnOnce(&peephole::Program) -> R,
    {
        k(&self.peephole_compile())
    }
}
