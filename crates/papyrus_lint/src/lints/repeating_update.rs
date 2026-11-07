//! `repeating-update`: `RegisterForUpdate`, whose updates keep coming whether
//! the previous one is done or not.
//!
//! ```papyrus
//! Event OnInit()
//!     RegisterForUpdate(1.0)         ; → RegisterForSingleUpdate(1.0)
//! EndEvent
//!
//! Event OnUpdate()
//!     Work()
//!     RegisterForSingleUpdate(1.0)   ; the next one, once this one is done
//! EndEvent
//! ```
//!
//! No fix: without a new registration in `OnUpdate`, the updates would stop.
//! Only the function of the game counts, not one a script declares.

use papyrus_ide::{
    Diagnostic,
    hir::Expr,
    index::Flags,
    sema::{Def, Sema},
};
use papyrus_intern::Name;

use crate::{Group, Lint, bodies};

const CODE: &str = "repeating-update";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Suspicious,
    summary: "`RegisterForUpdate`, whose updates can pile up",
    run,
    fix: None,
    manual_fix: false,
};

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    if !sema.index.mentions(Name::new("RegisterForUpdate")) {
        return;
    }
    for body in bodies(sema) {
        for (expr, range) in &body.exprs {
            let Expr::Call { name, def: Some(Def::Member { file, index }), .. } = expr else {
                continue;
            };
            if name.as_str().eq_ignore_ascii_case("RegisterForUpdate")
                && sema.member(*file, *index).flags.contains(Flags::NATIVE)
            {
                out.push(Diagnostic::warning(
                    *range,
                    CODE,
                    "updates can pile up: prefer `RegisterForSingleUpdate`, registered again at the end of `OnUpdate`",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::test_utils::check;

    #[test]
    fn reports_the_native_function_only() {
        let form = "ScriptName T\nFunction RegisterForUpdate(Float f) Native\nFunction RegisterForSingleUpdate(Float f) Native\nEvent OnInit()\n    RegisterForUpdate(1.0)\n    RegisterForSingleUpdate(1.0)\nEndEvent\n";
        assert_eq!(
            check("repeating-update", form),
            [
                "5: updates can pile up: prefer `RegisterForSingleUpdate`, registered again at the end of `OnUpdate`"
            ]
        );
        let own = "ScriptName T\nFunction RegisterForUpdate(Float f)\nEndFunction\nEvent OnInit()\n    RegisterForUpdate(1.0)\nEndEvent\n";
        assert_eq!(check("repeating-update", own), [""; 0]);
    }
}
