//! The intermediate representation of function bodies.
//!
//! Code generation produces one [`FunctionIr`] per function: instructions
//! with symbolic labels and operands by name. Optimization passes
//! ([`crate::opt`]) rewrite it, and the assembler (`script.rs`) resolves
//! labels to the relative jump offsets of the `.pex` format with
//! [`FunctionIr::label_positions`].
//!
//! Names are [`Name`]s: interned, `Copy`, and compared ignoring ASCII case
//! like Papyrus does, so copying an operand never allocates.

use std::fmt;

use papyrus_ide::index::TypeRef;
use papyrus_intern::Name;
use papyrus_pex::pex::OpCode;

/// A value used by an instruction.
#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    /// A variable, parameter, property or script name, `self`, `::NoneVar`, ...
    Var(Name),
    /// `None`
    None,
    /// An integer constant.
    Int(i32),
    /// A float constant.
    Float(f32),
    /// A boolean constant.
    Bool(bool),
    /// A string constant.
    Str(String),
}

impl Operand {
    /// A variable operand.
    pub fn var(name: &str) -> Operand {
        Operand::Var(Name::new(name))
    }

    /// Whether the operand is a constant.
    pub fn is_const(&self) -> bool {
        !matches!(self, Operand::Var(_))
    }

    /// The variable name, for variable operands.
    pub fn as_var(&self) -> Option<Name> {
        match self {
            Operand::Var(v) => Some(*v),
            _ => None,
        }
    }
}

/// A jump target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Label(pub u32);

/// An instruction of the IR.
#[derive(Clone, Debug, PartialEq)]
pub enum Inst {
    /// An instruction without jump: the opcode and all its arguments (for
    /// calls, the argument count is added when lowering).
    Op {
        /// The opcode.
        opcode: OpCode,
        /// The arguments.
        args: Vec<Operand>,
    },
    /// `jmp`, or `jmpt` / `jmpf` on `cond`.
    Jump {
        /// `Jmp`, `JmpT` or `JmpF`.
        opcode: OpCode,
        /// The condition of conditional jumps.
        cond: Option<Operand>,
        /// The target.
        target: Label,
    },
    /// The position of a label; no code.
    Label(Label),
}

/// A typed slot of a function: a parameter, a local or a temporary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    /// The name.
    pub name: Name,
    /// The type; `None` for the `None` type (`::NoneVar`).
    pub ty: Option<TypeRef>,
}

impl Slot {
    /// A slot named `name` of type `ty`.
    pub fn new(name: &str, ty: Option<TypeRef>) -> Slot {
        Slot { name: Name::new(name), ty }
    }
}

/// The code of one function, event or property accessor.
#[derive(Clone, Debug, Default)]
pub struct FunctionIr {
    /// Parameters.
    pub params: Vec<Slot>,
    /// Locals and temporaries, in creation order.
    pub locals: Vec<Slot>,
    /// Instructions with their source line (0 when unknown).
    pub code: Vec<(Inst, u32)>,
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Var(v) => f.write_str(v.as_str()),
            Operand::None => f.write_str("none"),
            Operand::Int(i) => write!(f, "{i}"),
            Operand::Float(x) => write!(f, "{x:?}"),
            Operand::Bool(b) => write!(f, "{b}"),
            Operand::Str(s) => write!(f, "{s:?}"),
        }
    }
}

/// The code as assembly, labels as `@N`, for logs.
impl fmt::Display for FunctionIr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (inst, line) in &self.code {
            match inst {
                Inst::Label(l) => writeln!(f, "  @{}:", l.0)?,
                Inst::Op { opcode, args } => {
                    write!(f, "    {}", opcode.mnemonic())?;
                    for a in args {
                        write!(f, " {a}")?;
                    }
                    writeln!(f, " ;@line {line}")?;
                }
                Inst::Jump { opcode, cond, target } => {
                    write!(f, "    {}", opcode.mnemonic())?;
                    if let Some(c) = cond {
                        write!(f, " {c}")?;
                    }
                    writeln!(f, " @{} ;@line {line}", target.0)?;
                }
            }
        }
        Ok(())
    }
}

impl FunctionIr {
    /// The number of instructions, labels excluded.
    pub fn instruction_count(&self) -> usize {
        self.code.iter().filter(|(i, _)| !matches!(i, Inst::Label(_))).count()
    }

    /// The instruction index of every label, by label number. A label that
    /// is never placed stands for the end of the code.
    pub fn label_positions(&self) -> Vec<usize> {
        let count = self
            .code
            .iter()
            .filter_map(|(inst, _)| match inst {
                Inst::Label(l) | Inst::Jump { target: l, .. } => Some(l.0 as usize + 1),
                Inst::Op { .. } => None,
            })
            .max()
            .unwrap_or(0);
        let mut positions = vec![None; count];
        let mut index = 0;
        for (inst, _) in &self.code {
            match inst {
                Inst::Label(l) => positions[l.0 as usize] = Some(index),
                _ => index += 1,
            }
        }
        positions.into_iter().map(|p| p.unwrap_or(index)).collect()
    }
}
