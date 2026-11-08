//! Optimization passes on [`FunctionIr`].
//!
//! - [`fold_constant_casts`]: `CAST ::tempN, 1` followed by uses of
//!   `::tempN` becomes the converted constant `1.0` in those uses, like the
//!   `-optimize` switch of the official compiler.
//! - [`remove_dead_jumps`]: drops jumps to the next instruction and code
//!   that cannot be reached after `RETURN` and `JUMP`.

use rustc_hash::FxHashMap;

use papyrus_ide::index::TypeRef;
use papyrus_intern::Name;
use papyrus_pex::pex::OpCode;

use crate::codegen::{
    constant,
    ir::{FunctionIr, Inst, Operand},
};

/// Runs all passes.
pub(crate) fn optimize(ir: &mut FunctionIr) {
    let passes: [(&str, fn(&mut FunctionIr)); 2] =
        [("fold_constant_casts", fold_constant_casts), ("remove_dead_jumps", remove_dead_jumps)];
    for (name, pass) in passes {
        let before = ir.instruction_count();
        pass(ir);
        crate::log::debug!("  {name}: {before} -> {} instructions", ir.instruction_count());
    }
}

/// How an instruction uses its argument at a position.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    /// A name (function, property, script): not a variable.
    Name,
    /// The written variable.
    Dest,
    /// A read value that may be a constant.
    Value,
    /// A read value that must be a variable (receivers, arrays, values the
    /// official compiler always passes in a variable).
    Var,
}

fn use_at(opcode: OpCode, pos: usize) -> Use {
    use OpCode::*;
    match (opcode, pos) {
        (CallMethod, 0) | (CallParent, 0) | (CallStatic, 0 | 1) => Use::Name,
        (PropGet | PropSet, 0) => Use::Name,
        (CallMethod | CallStatic, 2) | (CallParent, 1) => Use::Dest,
        (CallMethod, 1) => Use::Var,
        (PropGet, 1) | (PropSet, 1 | 2) => Use::Var,
        (PropGet, 2) => Use::Dest,
        (ArrayFindElement | ArrayRFindElement, 0) => Use::Var,
        (ArrayFindElement | ArrayRFindElement, 1) => Use::Dest,
        (ArraySetElement, 0 | 2) => Use::Var,
        (ArraySetElement, _) => Use::Value,
        (ArrayLength | ArrayGetElement, 1) => Use::Var,
        (CallMethod | CallStatic | CallParent | ArrayFindElement | ArrayRFindElement, _) => {
            Use::Value
        }
        (Ret, _) => Use::Value,
        (_, 0) => Use::Dest,
        _ => Use::Value,
    }
}

/// Replaces temporaries holding a cast constant by the converted constant.
pub(crate) fn fold_constant_casts(ir: &mut FunctionIr) {
    let types: FxHashMap<Name, Option<TypeRef>> =
        ir.locals.iter().map(|s| (s.name, s.ty)).collect();
    let mut i = 0;
    while i < ir.code.len() {
        if let Some((temp, folded)) = constant_cast(&ir.code[i].0, &types)
            && let Some(uses) = uses_in_block(ir, i, temp)
        {
            for (at, pos) in uses {
                match &mut ir.code[at].0 {
                    Inst::Op { args, .. } => args[pos] = folded.clone(),
                    Inst::Jump { cond, .. } => *cond = Some(folded.clone()),
                    Inst::Label(_) => {}
                }
            }
            ir.code.remove(i);
            continue;
        }
        i += 1;
    }
}

/// `CAST ::tempN, constant`: the temporary and the converted constant.
fn constant_cast(inst: &Inst, types: &FxHashMap<Name, Option<TypeRef>>) -> Option<(Name, Operand)> {
    let Inst::Op { opcode: OpCode::Cast, args } = inst else { return None };
    let [Operand::Var(temp), value] = args.as_slice() else { return None };
    let is_temp = temp
        .as_str()
        .strip_prefix("::temp")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if !is_temp || !value.is_const() {
        return None;
    }
    // A temporary of type `None` holds no constant worth folding.
    let to = (*types.get(temp)?)?;
    // Float to String depends on the formatting of the game; keep it.
    if matches!(value, Operand::Float(_)) && to == TypeRef::STRING {
        return None;
    }
    Some((*temp, constant::convert(value, to)?))
}

/// The uses of `temp` after its definition at `def`, when they are all in the
/// same basic block and accept a constant.
fn uses_in_block(ir: &FunctionIr, def: usize, temp: Name) -> Option<Vec<(usize, usize)>> {
    let is = |op: &Operand| op.as_var() == Some(temp);
    let mut uses = Vec::new();
    let mut end = ir.code.len();
    let mut redefined = false;
    for (j, (inst, _)) in ir.code.iter().enumerate().skip(def + 1) {
        match inst {
            Inst::Label(_) => {
                end = j;
                break;
            }
            Inst::Jump { cond, .. } => {
                if cond.as_ref().is_some_and(is) {
                    uses.push((j, 0));
                }
                end = j + 1;
                break;
            }
            Inst::Op { opcode, args } => {
                for (pos, arg) in args.iter().enumerate() {
                    match use_at(*opcode, pos) {
                        Use::Value if is(arg) => uses.push((j, pos)),
                        Use::Var if is(arg) => return None,
                        _ => {}
                    }
                }
                if args
                    .iter()
                    .enumerate()
                    .any(|(pos, a)| use_at(*opcode, pos) == Use::Dest && is(a))
                {
                    redefined = true;
                    end = j + 1;
                    break;
                }
            }
        }
    }
    if !redefined && live_in_any_block(ir, temp, def, end) {
        // The value may flow out of the block.
        return None;
    }
    Some(uses)
}

/// Whether some block (other than `def..end`) reads `temp` before writing
/// it, i.e. may see the value of another block.
fn live_in_any_block(ir: &FunctionIr, temp: Name, def: usize, end: usize) -> bool {
    let is = |op: &Operand| op.as_var() == Some(temp);
    // `None`: not seen yet in the current block; `Some(true)`: read first.
    let mut first_read: Option<bool> = None;
    for (j, (inst, _)) in ir.code.iter().enumerate() {
        if j == end {
            // A new block starts after the defining one.
            first_read = None;
        }
        if (def..end).contains(&j) {
            continue;
        }
        match inst {
            Inst::Label(_) => first_read = None,
            Inst::Jump { cond, .. } => {
                if first_read.is_none() && cond.as_ref().is_some_and(is) {
                    return true;
                }
                first_read = None;
            }
            Inst::Op { opcode, args } => {
                if first_read.is_some() {
                    continue;
                }
                let reads = args
                    .iter()
                    .enumerate()
                    .any(|(pos, a)| is(a) && matches!(use_at(*opcode, pos), Use::Value | Use::Var));
                if reads {
                    return true;
                }
                let writes = args
                    .iter()
                    .enumerate()
                    .any(|(pos, a)| is(a) && use_at(*opcode, pos) == Use::Dest);
                if writes {
                    first_read = Some(false);
                }
            }
        }
    }
    false
}

/// Removes jumps to the next instruction, and unreachable code after `RETURN`
/// and unconditional jumps (up to the next label).
pub(crate) fn remove_dead_jumps(ir: &mut FunctionIr) {
    let mut out: Vec<(Inst, u32)> = Vec::with_capacity(ir.code.len());
    let mut dead = false;
    for (inst, line) in ir.code.drain(..) {
        match &inst {
            Inst::Label(_) => dead = false,
            _ if dead => continue,
            Inst::Jump { opcode: OpCode::Jmp, .. } => dead = true,
            Inst::Op { opcode: OpCode::Ret, .. } => dead = true,
            _ => {}
        }
        out.push((inst, line));
    }
    // A jump followed only by labels, one of which is its target.
    let mut i = 0;
    while i < out.len() {
        if let Inst::Jump { target, .. } = &out[i].0 {
            let target = *target;
            let falls_through = out[i + 1..]
                .iter()
                .map_while(|(inst, _)| match inst {
                    Inst::Label(l) => Some(*l),
                    _ => None,
                })
                .any(|l| l == target);
            if falls_through {
                out.remove(i);
                continue;
            }
        }
        i += 1;
    }
    ir.code = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::ir::{Label, Slot};

    fn op(opcode: OpCode, args: Vec<Operand>) -> (Inst, u32) {
        (Inst::Op { opcode, args }, 1)
    }

    #[test]
    fn folds_int_to_float_cast() {
        let mut ir = FunctionIr {
            params: Vec::new(),
            locals: vec![
                Slot::new("::temp0", Some(TypeRef::FLOAT)),
                Slot::new("::temp1", Some(TypeRef::FLOAT)),
            ],
            code: vec![
                op(OpCode::Cast, vec![Operand::var("::temp0"), Operand::Int(1)]),
                op(
                    OpCode::FSub,
                    vec![Operand::var("::temp1"), Operand::var("r"), Operand::var("::temp0")],
                ),
                op(OpCode::Assign, vec![Operand::var("r"), Operand::var("::temp1")]),
            ],
        };
        optimize(&mut ir);
        assert_eq!(ir.code.len(), 2);
        assert_eq!(
            ir.code[0].0,
            Inst::Op {
                opcode: OpCode::FSub,
                args: vec![Operand::var("::temp1"), Operand::var("r"), Operand::Float(1.0)]
            }
        );
    }

    #[test]
    fn removes_jump_to_next() {
        let mut ir = FunctionIr {
            code: vec![
                op(OpCode::Ret, vec![Operand::None]),
                (Inst::Jump { opcode: OpCode::Jmp, cond: None, target: Label(0) }, 1),
                (Inst::Label(Label(0)), 1),
                op(OpCode::Ret, vec![Operand::None]),
            ],
            ..FunctionIr::default()
        };
        remove_dead_jumps(&mut ir);
        assert_eq!(ir.code.len(), 3);
    }
}
