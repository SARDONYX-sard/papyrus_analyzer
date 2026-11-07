//! `unreachable-code`: statements after a `Return` of the same block.
//!
//! ```papyrus
//! Function F()
//!     Return
//!     Debug.Trace("never")   ; unreachable
//! EndFunction
//! ```
//!
//! No fix: the code after the `Return` may be the part that is right.

use papyrus_ide::{
    Diagnostic,
    hir::{Body, Stmt, StmtId},
    sema::Sema,
};

use crate::{Group, Lint, bodies};

const CODE: &str = "unreachable-code";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Suspicious,
    summary: "statements after a `Return`",
    run,
    fix: None,
    manual_fix: false,
};

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    for body in bodies(sema) {
        block(&body, &body.root, out);
    }
}

fn block(body: &Body, stmts: &[StmtId], out: &mut Vec<Diagnostic>) {
    for (i, &s) in stmts.iter().enumerate() {
        match body.stmt(s) {
            Stmt::Return(_) if i + 1 < stmts.len() => {
                let range =
                    body.stmt_range(stmts[i + 1]).cover(body.stmt_range(stmts[stmts.len() - 1]));
                out.push(Diagnostic::warning(range, CODE, "unreachable code"));
                return;
            }
            Stmt::If { branches, else_branch } => {
                for (_, b) in branches {
                    block(body, b, out);
                }
                if let Some(b) = else_branch {
                    block(body, b, out);
                }
            }
            Stmt::While { body: b, .. } => block(body, b, out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::test_utils::check;

    #[test]
    fn reports_code_after_return() {
        let src = "ScriptName T\nFunction F(Bool b)\n    If b\n        Return\n        b = False\n    EndIf\n    Return\nEndFunction\n";
        assert_eq!(check("unreachable-code", src), ["5: unreachable code"]);
    }
}
