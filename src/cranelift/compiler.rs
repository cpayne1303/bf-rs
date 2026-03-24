use crate::common::{BfResult, Error};
use crate::peephole;
use crate::rts::{self, RtsState};
use crate::state::State;
use crate::traits::Interpretable;
use std::io::{Read, Write};
use std::mem;

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{AbiParam, BlockArg, InstBuilder, MemFlags, Value};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

pub struct Program {
    #[allow(dead_code)]
    module: JITModule,
    main_fn: *const u8,
}

// Safety: The pointer is to JIT-compiled code that is owned by the module.
unsafe impl Send for Program {}
unsafe impl Sync for Program {}

pub trait CraneliftCompilable {
    fn cranelift_compile(&self) -> Program;
}

impl<T: peephole::PeepholeCompilable + ?Sized> CraneliftCompilable for T {
    fn cranelift_compile(&self) -> Program {
        let ast = self.peephole_compile();
        compile(&ast)
    }
}

pub fn compile(program: &peephole::Program) -> Program {
    let mut flag_builder = settings::builder();
    flag_builder.set("use_colocated_libcalls", "false").unwrap();
    // FIXME: This is only needed for Windows because of how it handles symbols.
    flag_builder.set("is_pic", "false").unwrap();
    let isa_builder = cranelift_native::builder().unwrap_or_else(|msg| {
        panic!("host machine is not supported: {}", msg);
    });
    let isa = isa_builder
        .finish(settings::Flags::new(flag_builder))
        .unwrap();

    let mut module_builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());

    // Symbols for runtime system calls
    module_builder.symbol("rts_read", rts::RtsState::read as *const u8);
    module_builder.symbol("rts_write", rts::RtsState::write as *const u8);

    let mut module = JITModule::new(module_builder);

    let mut sig = module.make_signature();
    let ptr_type = module.target_config().pointer_type();

    // memory: *mut u8, memory_size: u64, rts_state: *mut RtsState
    sig.params.push(AbiParam::new(ptr_type));
    sig.params.push(AbiParam::new(ptr_type));
    sig.params.push(AbiParam::new(ptr_type));
    // returns u64 (status code)
    sig.returns.push(AbiParam::new(ptr_type));

    let func_id = module
        .declare_function("main", Linkage::Export, &sig)
        .unwrap();

    let mut ctx = module.make_context();
    ctx.func.signature = sig;

    let mut builder_context = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_context);
        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        builder.switch_to_block(entry_block);
        builder.seal_block(entry_block);

        // Arguments
        let mem_ptr = builder.block_params(entry_block)[0];
        let mem_size = builder.block_params(entry_block)[1];
        let rts_ptr = builder.block_params(entry_block)[2];

        // Variables
        let ptr_var = builder.declare_var(ptr_type);
        builder.def_var(ptr_var, mem_ptr);

        // We calculate mem_limit = mem_start + mem_size
        let mem_limit = builder.ins().iadd(mem_ptr, mem_size);

        let finish_block = builder.create_block();
        let _finish_param = builder.append_block_param(finish_block, ptr_type);
        let underflow_block = builder.create_block();
        let overflow_block = builder.create_block();

        let mut compiler = Compiler {
            builder,
            module: &mut module,
            ptr_type,
            mem_start: mem_ptr,
            mem_limit,
            rts_ptr,
            ptr_var,
            finish_block,
            underflow_block,
            overflow_block,
        };

        compiler.compile(program);

        let okay = compiler.builder.ins().iconst(ptr_type, rts::OKAY as i64);
        let args = [BlockArg::Value(okay)];
        compiler.builder.ins().jump(compiler.finish_block, &args);

        // Epilogue blocks
        compiler.builder.switch_to_block(compiler.underflow_block);
        let underflow = compiler
            .builder
            .ins()
            .iconst(ptr_type, rts::UNDERFLOW as i64);
        let args = [BlockArg::Value(underflow)];
        compiler.builder.ins().jump(compiler.finish_block, &args);

        compiler.builder.switch_to_block(compiler.overflow_block);
        let overflow = compiler
            .builder
            .ins()
            .iconst(ptr_type, rts::OVERFLOW as i64);
        let args = [BlockArg::Value(overflow)];
        compiler.builder.ins().jump(compiler.finish_block, &args);

        compiler.builder.switch_to_block(compiler.finish_block);
        let result = compiler.builder.block_params(compiler.finish_block)[0];
        compiler.builder.ins().return_(&[result]);

        compiler.builder.seal_block(compiler.underflow_block);
        compiler.builder.seal_block(compiler.overflow_block);
        compiler.builder.seal_block(compiler.finish_block);

        compiler.builder.finalize();
    }

    module.define_function(func_id, &mut ctx).unwrap();
    module.finalize_definitions().unwrap();

    let code = module.get_finalized_function(func_id);

    Program {
        module,
        main_fn: code,
    }
}

struct Compiler<'a> {
    builder: FunctionBuilder<'a>,
    module: &'a mut JITModule,
    ptr_type: types::Type,
    mem_start: Value,
    mem_limit: Value,
    rts_ptr: Value,
    ptr_var: Variable,
    finish_block: cranelift_codegen::ir::Block,
    underflow_block: cranelift_codegen::ir::Block,
    overflow_block: cranelift_codegen::ir::Block,
}

impl<'a> Compiler<'a> {
    fn compile(&mut self, program: &[peephole::Statement]) {
        for stmt in program {
            self.compile_statement(stmt);
        }
    }

    fn compile_statement(&mut self, stmt: &peephole::Statement) {
        use crate::common::Instruction::*;
        use peephole::Statement::*;

        match stmt {
            Instr(Right(count)) => {
                let ptr = self.builder.use_var(self.ptr_var);
                let new_ptr = self.builder.ins().iadd_imm(ptr, *count as i64);
                self.builder.def_var(self.ptr_var, new_ptr);

                let cond = self.builder.ins().icmp(
                    IntCC::UnsignedGreaterThanOrEqual,
                    new_ptr,
                    self.mem_limit,
                );
                let next_block = self.builder.create_block();
                let no_args: [BlockArg; 0] = [];
                self.builder
                    .ins()
                    .brif(cond, self.overflow_block, &no_args, next_block, &no_args);
                self.builder.switch_to_block(next_block);
                self.builder.seal_block(next_block);
            }
            Instr(Left(count)) => {
                let ptr = self.builder.use_var(self.ptr_var);
                let new_ptr = self.builder.ins().iadd_imm(ptr, -(*count as i64));
                self.builder.def_var(self.ptr_var, new_ptr);

                let cond =
                    self.builder
                        .ins()
                        .icmp(IntCC::UnsignedLessThan, new_ptr, self.mem_start);
                let next_block = self.builder.create_block();
                let no_args: [BlockArg; 0] = [];
                self.builder
                    .ins()
                    .brif(cond, self.underflow_block, &no_args, next_block, &no_args);
                self.builder.switch_to_block(next_block);
                self.builder.seal_block(next_block);
            }
            Instr(Add(count)) => {
                let ptr = self.builder.use_var(self.ptr_var);
                let val = self.builder.ins().load(types::I8, MemFlags::new(), ptr, 0);
                let added = self.builder.ins().iadd_imm(val, *count as i64);
                self.builder.ins().store(MemFlags::new(), added, ptr, 0);
            }
            Instr(SetZero) => {
                let ptr = self.builder.use_var(self.ptr_var);
                let zero = self.builder.ins().iconst(types::I8, 0);
                self.builder.ins().store(MemFlags::new(), zero, ptr, 0);
            }
            Instr(Out) => {
                let ptr = self.builder.use_var(self.ptr_var);
                let val = self.builder.ins().load(types::I8, MemFlags::new(), ptr, 0);
                let val32 = self.builder.ins().uextend(types::I32, val);
                self.call_rts("rts_write", &[self.rts_ptr, val32]);
            }
            Instr(In) => {
                let ptr = self.builder.use_var(self.ptr_var);
                let res = self.call_rts("rts_read", &[self.rts_ptr]);
                let res8 = self.builder.ins().ireduce(types::I8, res);
                self.builder.ins().store(MemFlags::new(), res8, ptr, 0);
            }
            Loop(body) => {
                let header = self.builder.create_block();
                let body_block = self.builder.create_block();
                let exit_block = self.builder.create_block();

                self.builder.ins().jump(header, &[]);
                self.builder.switch_to_block(header);

                let ptr = self.builder.use_var(self.ptr_var);
                let val = self.builder.ins().load(types::I8, MemFlags::new(), ptr, 0);
                let cond = self.builder.ins().icmp_imm(IntCC::NotEqual, val, 0);
                let no_args: [BlockArg; 0] = [];
                self.builder
                    .ins()
                    .brif(cond, body_block, &no_args, exit_block, &no_args);

                self.builder.switch_to_block(body_block);
                self.compile(body);
                self.builder.ins().jump(header, &[]);

                self.builder.switch_to_block(exit_block);
                self.builder.seal_block(header);
                self.builder.seal_block(body_block);
                self.builder.seal_block(exit_block);
            }
            Instr(FindZeroRight(skip)) => {
                let header = self.builder.create_block();
                let body_block = self.builder.create_block();
                let exit_block = self.builder.create_block();

                self.builder.ins().jump(header, &[]);
                self.builder.switch_to_block(header);
                let ptr = self.builder.use_var(self.ptr_var);
                let val = self.builder.ins().load(types::I8, MemFlags::new(), ptr, 0);
                let cond = self.builder.ins().icmp_imm(IntCC::NotEqual, val, 0);
                let no_args: [BlockArg; 0] = [];
                self.builder
                    .ins()
                    .brif(cond, body_block, &no_args, exit_block, &no_args);

                self.builder.switch_to_block(body_block);
                let ptr = self.builder.use_var(self.ptr_var);
                let new_ptr = self.builder.ins().iadd_imm(ptr, *skip as i64);
                self.builder.def_var(self.ptr_var, new_ptr);
                self.builder.ins().jump(header, &[]);

                self.builder.switch_to_block(exit_block);
                self.builder.seal_block(header);
                self.builder.seal_block(body_block);
                self.builder.seal_block(exit_block);
            }
            Instr(FindZeroLeft(skip)) => {
                let header = self.builder.create_block();
                let body_block = self.builder.create_block();
                let exit_block = self.builder.create_block();

                self.builder.ins().jump(header, &[]);
                self.builder.switch_to_block(header);
                let ptr = self.builder.use_var(self.ptr_var);
                let val = self.builder.ins().load(types::I8, MemFlags::new(), ptr, 0);
                let cond = self.builder.ins().icmp_imm(IntCC::NotEqual, val, 0);
                let no_args: [BlockArg; 0] = [];
                self.builder
                    .ins()
                    .brif(cond, body_block, &no_args, exit_block, &no_args);

                self.builder.switch_to_block(body_block);
                let ptr = self.builder.use_var(self.ptr_var);
                let new_ptr = self.builder.ins().iadd_imm(ptr, -(*skip as i64));
                self.builder.def_var(self.ptr_var, new_ptr);
                self.builder.ins().jump(header, &[]);

                self.builder.switch_to_block(exit_block);
                self.builder.seal_block(header);
                self.builder.seal_block(body_block);
                self.builder.seal_block(exit_block);
            }
            Instr(OffsetAddRight(offset)) => {
                let skip_block = self.builder.create_block();
                let body_block = self.builder.create_block();

                let ptr = self.builder.use_var(self.ptr_var);
                let val = self.builder.ins().load(types::I8, MemFlags::new(), ptr, 0);
                let cond = self.builder.ins().icmp_imm(IntCC::NotEqual, val, 0);
                let no_args: [BlockArg; 0] = [];
                self.builder
                    .ins()
                    .brif(cond, body_block, &no_args, skip_block, &no_args);

                self.builder.switch_to_block(body_block);
                let ptr = self.builder.use_var(self.ptr_var);
                let target_ptr = self.builder.ins().iadd_imm(ptr, *offset as i64);
                let target_val = self
                    .builder
                    .ins()
                    .load(types::I8, MemFlags::new(), target_ptr, 0);
                let new_val = self.builder.ins().iadd(target_val, val);
                self.builder
                    .ins()
                    .store(MemFlags::new(), new_val, target_ptr, 0);
                let zero = self.builder.ins().iconst(types::I8, 0);
                self.builder.ins().store(MemFlags::new(), zero, ptr, 0);
                self.builder.ins().jump(skip_block, &[]);

                self.builder.switch_to_block(skip_block);
                self.builder.seal_block(body_block);
                self.builder.seal_block(skip_block);
            }
            Instr(OffsetAddLeft(offset)) => {
                let skip_block = self.builder.create_block();
                let body_block = self.builder.create_block();

                let ptr = self.builder.use_var(self.ptr_var);
                let val = self.builder.ins().load(types::I8, MemFlags::new(), ptr, 0);
                let cond = self.builder.ins().icmp_imm(IntCC::NotEqual, val, 0);
                let no_args: [BlockArg; 0] = [];
                self.builder
                    .ins()
                    .brif(cond, body_block, &no_args, skip_block, &no_args);

                self.builder.switch_to_block(body_block);
                let ptr = self.builder.use_var(self.ptr_var);
                let target_ptr = self.builder.ins().iadd_imm(ptr, -(*offset as i64));
                let target_val = self
                    .builder
                    .ins()
                    .load(types::I8, MemFlags::new(), target_ptr, 0);
                let new_val = self.builder.ins().iadd(target_val, val);
                self.builder
                    .ins()
                    .store(MemFlags::new(), new_val, target_ptr, 0);
                let zero = self.builder.ins().iconst(types::I8, 0);
                self.builder.ins().store(MemFlags::new(), zero, ptr, 0);
                self.builder.ins().jump(skip_block, &[]);

                self.builder.switch_to_block(skip_block);
                self.builder.seal_block(body_block);
                self.builder.seal_block(skip_block);
            }
            _ => {
                // Ignore unimplemented peephole instructions for now
            }
        }
    }

    fn call_rts(&mut self, name: &str, args: &[Value]) -> Value {
        let mut sig = self.module.make_signature();
        for arg in args {
            sig.params
                .push(AbiParam::new(self.builder.func.dfg.value_type(*arg)));
        }
        sig.returns.push(AbiParam::new(self.ptr_type));

        let callee = self
            .module
            .declare_function(name, Linkage::Import, &sig)
            .unwrap();
        let local_callee = self
            .module
            .declare_func_in_func(callee, &mut self.builder.func);
        let call = self.builder.ins().call(local_callee, args);
        self.builder.inst_results(call)[0]
    }
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::assert_interpret;
    use crate::traits::CraneliftCompilable;

    #[test]
    fn hello_world() {
        let program = crate::ast::parse_program(crate::test_helpers::HELLO_WORLD_SRC).unwrap();
        let program = program.cranelift_compile();
        assert_interpret(&program, b"", b"Hello, World!");
    }

    #[test]
    fn factoring() {
        let program = crate::ast::parse_program(crate::test_helpers::FACTOR_SRC).unwrap();
        let program = program.cranelift_compile();
        assert_interpret(&program, b"100\n", b"100: 2 2 5 5\n");
    }
}

type EntryFunction =
    extern "C" fn(memory: *mut u8, memory_size: u64, rts_state: *mut RtsState) -> u64;

impl Interpretable for Program {
    fn interpret_state<R: Read, W: Write>(
        &self,
        mut state: State,
        mut input: R,
        mut output: W,
    ) -> BfResult<()> {
        let mut rts_state = RtsState::new(&mut input, &mut output);
        let main_fn: EntryFunction = unsafe { mem::transmute(self.main_fn) };

        let result = main_fn(state.as_mut_ptr(), state.capacity() as u64, &mut rts_state);

        match result {
            rts::OKAY => Ok(()),
            rts::UNDERFLOW => Err(Error::PointerUnderflow),
            rts::OVERFLOW => Err(Error::PointerOverflow),
            _ => panic!("Unknown result code: {}", result),
        }
    }
}
