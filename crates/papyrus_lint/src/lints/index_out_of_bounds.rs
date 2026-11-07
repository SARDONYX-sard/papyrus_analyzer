//! `index-out-of-bounds`: a constant index outside of an array whose length
//! is known, like clippy's `out_of_bounds_indexing`.
//!
//! ```papyrus
//! Function F()
//!     Int[] values = New Int[4]
//!     values[4] = 1     ; `values` has 4 elements (0-3)
//! EndFunction
//! ```
//!
//! The compiler accepts it; in the game the access fails with "Array index 4
//! is out of range (0-3)" in the log, and reads give the default value.
//!
//! The length is known for a local variable (not a parameter) whose every
//! assignment is `New Type[size]` with the same size: an array never changes
//! length, and only assignments change which array a variable holds.

use papyrus_ide::{
    Diagnostic,
    hir::{Body, Expr, ExprId, Literal, LocalId, Stmt, UnaryOp},
    sema::Sema,
};

use crate::{Group, Lint, bodies};

const CODE: &str = "index-out-of-bounds";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Correctness,
    summary: "a constant index outside of an array of known length",
    run,
    fix: None,
    manual_fix: false,
};

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    for body in bodies(sema) {
        let lengths = known_lengths(sema, &body);
        for (expr, _) in &body.exprs {
            let Expr::Index { base, index } = expr else { continue };
            let Expr::Local(local) = body.expr(*base) else { continue };
            let Some(len) = lengths[local.0 as usize] else { continue };
            let Some(i) = constant(sema, &body, *index) else { continue };
            if (0..len).contains(&i) {
                continue;
            }
            out.push(Diagnostic::error(
                body.expr_range(*index),
                CODE,
                format!(
                    "index {i} is out of bounds: `{}` has {len} element{} ({})",
                    body.local(*local).name,
                    if len == 1 { "" } else { "s" },
                    if len == 1 { "0".to_owned() } else { format!("0-{}", len - 1) }
                ),
            ));
        }
    }
}

/// The length of the array of each local, when every assignment is a `New`
/// of the same size.
fn known_lengths(sema: &Sema<'_>, body: &Body) -> Vec<Option<i64>> {
    // `None`: not assigned yet; `Some(None)`: unknown.
    let mut lengths: Vec<Option<Option<i64>>> = vec![None; body.locals.len()];
    let mut assign = |local: LocalId, value: Option<ExprId>| {
        let size = value.and_then(|v| match body.expr(v) {
            Expr::New { size, .. } => constant(sema, body, *size),
            _ => None,
        });
        let slot = &mut lengths[local.0 as usize];
        *slot = match *slot {
            None => Some(size),
            Some(known) if known == size => Some(known),
            Some(_) => Some(None),
        };
    };
    for (stmt, _) in &body.stmts {
        match stmt {
            Stmt::Local { local, init } => assign(*local, *init),
            Stmt::Assign { op, target, value } => {
                if let Expr::Local(local) = body.expr(*target) {
                    assign(*local, op.is_none().then_some(*value));
                }
            }
            _ => {}
        }
    }
    (lengths.into_iter().enumerate())
        .map(|(i, len)| len.flatten().filter(|_| !body.locals[i].is_param))
        .collect()
}

/// The value of an integer literal, possibly negated.
fn constant(sema: &Sema<'_>, body: &Body, expr: ExprId) -> Option<i64> {
    match body.expr(expr) {
        Expr::Literal(Literal::Int) => {
            let text = &sema.text[body.expr_range(expr)];
            match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                Some(hex) => u32::from_str_radix(hex, 16).ok().map(|v| i64::from(v as i32)),
                None => text.parse().ok(),
            }
        }
        Expr::Unary { op: UnaryOp::Neg, expr } => constant(sema, body, *expr).map(|v| -v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::test_utils::check;

    #[test]
    fn reports_constant_indexes_outside_of_new_arrays() {
        let src = "ScriptName T\nFunction F(Int[] akParam)\n    Int[] a = New Int[4]\n    a[3] = 1\n    a[4] = 1\n    Int x = a[-1]\n    Int[] b = New Int[1]\n    b = New Int[1]\n    x = b[0x1]\n    akParam[9] = 1\n    Int[] c = New Int[2]\n    c = New Int[5]\n    c[4] = 1\n    Int i = 7\n    a[i] = 1\nEndFunction\n";
        assert_eq!(
            check("index-out-of-bounds", src),
            [
                "5: index 4 is out of bounds: `a` has 4 elements (0-3)",
                "6: index -1 is out of bounds: `a` has 4 elements (0-3)",
                "9: index 1 is out of bounds: `b` has 1 element (0)",
            ]
        );
    }
}
