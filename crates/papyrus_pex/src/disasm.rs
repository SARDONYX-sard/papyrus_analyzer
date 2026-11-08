//! Disassembly of `.pex` files into text.
//!
//! The format follows the `.pas` assembly of the official PapyrusAssembler:
//! nested `.object`, `.state`, `.function` and `.code` blocks. Jump offsets
//! are shown as `labelN` and the targets as `labelN:` lines; the source line
//! of each instruction, when known, follows as `;@line N`.
//!
//! Everything is written straight into one output `String`: names and values
//! are formatted through [`fmt::Display`] values, never through temporary
//! `String`s.

use std::{
    borrow::Cow,
    fmt::{self, Write as _},
};

use crate::pex::*;

/// Options of [`disassemble`].
#[derive(Clone, Copy, Debug, Default)]
pub struct DisasmOptions {
    /// Leave out the header (file names, times, user and machine), which
    /// differs between builds of the same source.
    pub skip_header: bool,
    /// Leave out the `;@line` comments.
    pub skip_lines: bool,
}

/// Disassembles a file.
pub fn disassemble(pex: &PexFile<'_>, options: DisasmOptions) -> String {
    let mut d = Disasm { pex, options, out: String::new(), indent: 0 };
    d.file();
    d.out
}

struct Disasm<'p, 'a> {
    pex: &'p PexFile<'a>,
    options: DisasmOptions,
    out: String,
    indent: usize,
}

/// Output: indented lines, and blocks that indent what they contain.
impl Disasm<'_, '_> {
    fn line(&mut self, text: fmt::Arguments<'_>) {
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        let _ = self.out.write_fmt(text);
        self.out.push('\n');
    }

    /// A line starting a block: the following lines are indented.
    fn open(&mut self, text: fmt::Arguments<'_>) {
        self.line(text);
        self.indent += 1;
    }

    /// The line ending a block.
    fn close(&mut self, text: &str) {
        self.indent -= 1;
        self.line(format_args!("{text}"));
    }

    /// `labelN:`, one level left of the code.
    fn label(&mut self, n: usize) {
        self.indent -= 1;
        self.line(format_args!("label{n}:"));
        self.indent += 1;
    }
}

impl<'p> Disasm<'p, '_> {
    /// A string of the string table, as is.
    fn s(&self, id: StringId) -> Cow<'p, str> {
        self.pex.str(id)
    }

    fn file(&mut self) {
        let pex = self.pex;
        if !self.options.skip_header {
            self.open(format_args!(".info"));
            self.line(format_args!(".source {}", quoted(&pex.src_file_name.decode())));
            if let Some(debug) = &pex.debug_info {
                self.line(format_args!(".modifyTime {}", debug.modification_time));
            }
            self.line(format_args!(".compileTime {}", pex.compilation_time));
            self.line(format_args!(".user {}", quoted(&pex.user_name.decode())));
            self.line(format_args!(".computer {}", quoted(&pex.machine_name.decode())));
            self.line(format_args!(".version {}.{}", pex.major_version, pex.minor_version));
            self.close(".endInfo");
        }
        self.open(format_args!(".userFlagsRef"));
        let mut flags: Vec<&UserFlag> = pex.user_flags.iter().collect();
        flags.sort_by_key(|f| f.flag_index);
        for flag in flags {
            self.line(format_args!(".flag {} {}", self.s(flag.name), flag.flag_index));
        }
        self.close(".endUserFlagsRef");
        self.open(format_args!(".objectTable"));
        for object in &pex.objects {
            self.object(object);
        }
        self.close(".endObjectTable");
    }

    fn object(&mut self, object: &Object) {
        self.open(format_args!(
            ".object {} {}",
            self.s(object.name),
            self.s(object.parent_class_name)
        ));
        self.line(format_args!(".userFlags {}", object.user_flags.bits()));
        self.line(format_args!(".docString {}", quoted(&self.s(object.docstring))));
        self.line(format_args!(".autoState {}", self.s(object.auto_state_name)));

        self.open(format_args!(".variableTable"));
        for v in &object.variables {
            self.open(format_args!(".variable {} {}", self.s(v.name), self.s(v.type_name)));
            self.line(format_args!(".userFlags {}", v.user_flags));
            self.line(format_args!(".initialValue {}", value(self.pex, &v.value)));
            self.close(".endVariable");
        }
        self.close(".endVariableTable");

        self.open(format_args!(".propertyTable"));
        for p in &object.properties {
            let auto = if p.is_auto_var() { " auto" } else { "" };
            self.open(format_args!(".property {} {}{auto}", self.s(p.name), self.s(p.ty)));
            self.line(format_args!(".userFlags {}", p.user_flags));
            self.line(format_args!(".docString {}", quoted(&self.s(p.docstring))));
            if let Some(var) = p.auto_var_name {
                self.line(format_args!(".autoVar {}", self.s(var)));
            }
            if let Some(get) = &p.read_handler {
                let lines = self.debug_lines(object, None, p.name, DebugFunctionType::Getter);
                self.function("get", get, lines);
            }
            if let Some(set) = &p.write_handler {
                let lines = self.debug_lines(object, None, p.name, DebugFunctionType::Setter);
                self.function("set", set, lines);
            }
            self.close(".endProperty");
        }
        self.close(".endPropertyTable");

        self.open(format_args!(".stateTable"));
        for state in &object.states {
            let name = self.s(state.name);
            // The empty state has no name.
            let space = if name.is_empty() { "" } else { " " };
            self.open(format_args!(".state{space}{name}"));
            for f in &state.functions {
                let lines =
                    self.debug_lines(object, Some(state.name), f.name, DebugFunctionType::Method);
                self.function(&self.s(f.name), &f.info, lines);
            }
            self.close(".endState");
        }
        self.close(".endStateTable");
        self.close(".endObject");
    }

    /// The source line of each instruction of a function, from the debug
    /// information. Accessors match any state (`state: None`).
    fn debug_lines(
        &self,
        object: &Object,
        state: Option<StringId>,
        name: StringId,
        ty: DebugFunctionType,
    ) -> Option<&'p [u16]> {
        let debug = self.pex.debug_info.as_ref()?;
        debug
            .functions
            .iter()
            .find(|f| {
                f.object_name == object.name
                    && f.function_name == name
                    && f.function_type == ty
                    && state.is_none_or(|s| f.state_name == s)
            })
            .map(|f| f.instruction_line_numbers.as_slice())
    }

    fn function(&mut self, name: &str, info: &FunctionInfo, lines: Option<&[u16]>) {
        let global = if info.is_global() { " static" } else { "" };
        let native = if info.is_native() { " native" } else { "" };
        self.open(format_args!(".function {name}{global}{native}"));
        self.line(format_args!(".userFlags {}", info.user_flags));
        self.line(format_args!(".docString {}", quoted(&self.s(info.docstring))));
        self.line(format_args!(".return {}", self.s(info.return_type)));
        self.open(format_args!(".paramTable"));
        for p in &info.params {
            self.line(format_args!(".param {} {}", self.s(p.name), self.s(p.ty)));
        }
        self.close(".endParamTable");
        self.open(format_args!(".localTable"));
        for l in &info.locals {
            self.line(format_args!(".local {} {}", self.s(l.name), self.s(l.ty)));
        }
        self.close(".endLocalTable");
        self.open(format_args!(".code"));
        self.code(&info.instructions, lines);
        self.close(".endCode");
        self.close(".endFunction");
    }

    fn code(&mut self, code: &[Instruction], lines: Option<&[u16]>) {
        // The jump targets, numbered in order of position.
        let mut targets: Vec<usize> =
            code.iter().enumerate().filter_map(|(i, ins)| jump_target(i, ins)).collect();
        targets.sort_unstable();
        targets.dedup();
        let label_of = |target: usize| targets.binary_search(&target).unwrap_or(0);

        let pex = self.pex;
        for (i, ins) in code.iter().enumerate() {
            if targets.binary_search(&i).is_ok() {
                self.label(label_of(i));
            }
            let jump = jump_target(i, ins);
            let args = fmt::from_fn(|f| {
                for (n, arg) in ins.args.iter().enumerate() {
                    match jump {
                        Some(t) if is_jump_offset(ins.opcode, n) => {
                            write!(f, " label{}", label_of(t))?
                        }
                        _ => write!(f, " {}", value(pex, arg))?,
                    }
                }
                Ok(())
            });
            let line = match lines.and_then(|l| l.get(i)) {
                Some(line) if !self.options.skip_lines => Some(line),
                _ => None,
            };
            let line = fmt::from_fn(|f| match line {
                Some(line) => write!(f, " ;@line {line}"),
                None => Ok(()),
            });
            self.line(format_args!("{}{args}{line}", ins.opcode.mnemonic()));
        }
        // A jump past the last instruction.
        if targets.last().is_some_and(|&t| t >= code.len()) {
            self.label(label_of(code.len()));
        }
    }
}

/// The instruction index a jump goes to: offsets are relative to the jump.
pub fn jump_target(index: usize, ins: &Instruction) -> Option<usize> {
    let offset = match (ins.opcode, ins.args.as_slice()) {
        (OpCode::Jmp, [VariableValue::Integer(o)]) => *o,
        (OpCode::JmpT | OpCode::JmpF, [_, VariableValue::Integer(o)]) => *o,
        _ => return None,
    };
    usize::try_from(index as i64 + offset as i64).ok()
}

/// Whether argument `n` of an instruction is a jump offset: `jmp offset`,
/// `jmpt cond, offset`, `jmpf cond, offset`.
fn is_jump_offset(opcode: OpCode, n: usize) -> bool {
    matches!((opcode, n), (OpCode::Jmp, 0) | (OpCode::JmpT | OpCode::JmpF, 1))
}

/// An operand as assembly: names as is, strings quoted.
fn value<'p>(pex: &'p PexFile<'_>, value: &VariableValue) -> impl fmt::Display + 'p {
    let value = *value;
    fmt::from_fn(move |f| match value {
        VariableValue::Null => f.write_str("none"),
        VariableValue::Identifier(id) => f.write_str(&pex.str(id)),
        VariableValue::String(id) => write!(f, "{}", quoted(&pex.str(id))),
        VariableValue::Integer(i) => write!(f, "{i}"),
        VariableValue::Float(x) => write!(f, "{x:?}"),
        VariableValue::Boolean(b) => write!(f, "{b}"),
    })
}

/// `s` as a string literal: quoted, with `"`, `\`, newlines and tabs escaped.
fn quoted(s: &str) -> impl fmt::Display + '_ {
    fmt::from_fn(move |f| {
        f.write_char('"')?;
        for c in s.chars() {
            match c {
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                '\n' => f.write_str("\\n")?,
                '\t' => f.write_str("\\t")?,
                c => f.write_char(c)?,
            }
        }
        f.write_char('"')
    })
}
