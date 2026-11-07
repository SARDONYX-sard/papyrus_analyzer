//! `unused-script-variable`: a script variable whose value is never read.
//!
//! ```papyrus
//! Int count         ; `count` is never read
//!
//! Function F()
//!     count = 1
//! EndFunction
//! ```
//!
//! Script variables are private: only their script can read them, so one file
//! is enough to tell. Variables flagged `Conditional` are read by the
//! conditions of the game, and properties by other scripts: not reported.
//!
//! No fix: the writes may have effects to keep, and the value may be meant to
//! be read.

use papyrus_ide::{
    Diagnostic,
    hir::{Body, Expr, Stmt},
    index::MemberKind,
    sema::{Def, Sema},
};

use crate::{Group, Lint, bodies};

const CODE: &str = "unused-script-variable";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Suspicious,
    summary: "a script variable whose value is never read",
    run,
    fix: None,
    manual_fix: false,
};

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    let members = &sema.index.members;
    if !members.iter().any(|m| m.kind == MemberKind::Variable) {
        return;
    }
    let mut read = vec![false; members.len()];
    for body in bodies(sema) {
        mark_reads(sema, &body, &mut read);
    }
    for (i, member) in members.iter().enumerate() {
        let conditional =
            member.user_flags.iter().any(|f| f.as_str().eq_ignore_ascii_case("Conditional"));
        if member.kind == MemberKind::Variable && !read[i] && !conditional {
            out.push(Diagnostic::warning(
                member.name_range,
                CODE,
                format!("`{}` is never read", member.name),
            ));
        }
    }
}

/// Targets of `=` are writes; every other use is a read (`x += 1` too).
fn mark_reads(sema: &Sema<'_>, body: &Body, read: &mut [bool]) {
    let mut targets = vec![false; body.exprs.len()];
    for (stmt, _) in &body.stmts {
        if let Stmt::Assign { op: None, target, .. } = stmt {
            targets[target.0 as usize] = true;
        }
    }
    for (i, (expr, _)) in body.exprs.iter().enumerate() {
        let def = match expr {
            Expr::Def(def) => def,
            Expr::Field { def: Some(def), .. } => def,
            _ => continue,
        };
        if let Def::Member { file, index } = *def
            && file == sema.file
            && !targets[i]
        {
            read[index as usize] = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::test_utils::check;

    #[test]
    fn reports_variables_never_read() {
        let src = "ScriptName T\nInt never\nInt written\nInt read\nInt compound\nInt[] arr\nInt viaSelf\nInt Property Prop Auto\nFunction F()\n    written = 1\n    Debug.Trace(read)\n    compound += 1\n    arr[0] = 1\n    Debug.Trace(Self.viaSelf)\nEndFunction\n";
        assert_eq!(
            check("unused-script-variable", src),
            ["2: `never` is never read", "3: `written` is never read"]
        );
    }

    #[test]
    fn conditional_variables_are_read_by_the_game() {
        let src =
            "ScriptName T Conditional\nInt v Conditional\nFunction F()\n    v = 1\nEndFunction\n";
        assert_eq!(check("unused-script-variable", src), [""; 0]);
    }

    #[test]
    fn a_local_of_the_same_name_is_another_variable() {
        let src =
            "ScriptName T\nInt v\nFunction F()\n    Int v = 1\n    Debug.Trace(v)\nEndFunction\n";
        assert_eq!(check("unused-script-variable", src), ["2: `v` is never read"]);
    }

    #[test]
    fn reads_in_property_functions_and_states_count() {
        let src = "ScriptName T\nInt a\nInt b\nInt Property P\n    Int Function Get()\n        Return a\n    EndFunction\nEndProperty\nState S\n    Function F()\n        Debug.Trace(b)\n    EndFunction\nEndState\n";
        assert_eq!(check("unused-script-variable", src), [""; 0]);
    }
}
