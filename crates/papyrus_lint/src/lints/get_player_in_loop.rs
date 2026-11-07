//! `get-player-in-loop`: `Game.GetPlayer()` called on every turn of a loop.
//!
//! ```papyrus
//! While i < 10
//!     Game.GetPlayer().AddItem(item)   ; called 10 times
//!     i += 1
//! EndWhile
//! ```
//!
//! The player does not change: a variable set before the loop, or a property
//! filled with `PlayerRef`, needs no call. The Creation Kit wiki advises the
//! function only to access the player once. Pedantic: it matters for loops
//! running often only.
//!
//! No fix: where the variable goes is up to the author.

use papyrus_ide::{
    Diagnostic,
    hir::{Expr, Stmt},
    sema::{Def, Sema},
};
use papyrus_intern::Name;

use crate::{Group, Lint, bodies};

const CODE: &str = "get-player-in-loop";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Pedantic,
    summary: "`Game.GetPlayer()` called in a loop",
    run,
    fix: None,
    manual_fix: false,
};

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    if !sema.index.mentions(Name::new("GetPlayer")) {
        return;
    }
    for body in bodies(sema) {
        for (expr, range) in &body.exprs {
            let Expr::Call { name, def: Some(Def::Member { file, .. }), .. } = expr else {
                continue;
            };
            let is_game = sema.ws.file(*file).index.name_str().eq_ignore_ascii_case("Game");
            if is_game
                && name.as_str().eq_ignore_ascii_case("GetPlayer")
                && body
                    .stmts
                    .iter()
                    .any(|(s, r)| matches!(s, Stmt::While { .. }) && r.contains_range(*range))
            {
                out.push(Diagnostic::warning(
                    *range,
                    CODE,
                    "`GetPlayer()` runs on every turn: keep it in a variable before the loop",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use papyrus_ide::Workspace;

    use crate::{Level, LintConfig};

    fn check(src: &str) -> Vec<String> {
        let mut ws = Workspace::new();
        ws.set_open(
            "/ws/Game.psc".into(),
            "ScriptName Game Hidden\nT Function GetPlayer() Native Global\n".into(),
            0,
        );
        let file = ws.set_open("/ws/T.psc".into(), src.into(), 0);
        let mut config = LintConfig::default();
        config.set("all", Level::Allow).unwrap();
        config.set("get-player-in-loop", Level::Warn).unwrap();
        let diags = crate::lint(&ws, file, &config);
        let line = |d: &papyrus_ide::Diagnostic| {
            src[..usize::from(d.range.start())].matches('\n').count() + 1
        };
        diags.iter().map(|d| line(d).to_string()).collect()
    }

    #[test]
    fn reports_calls_in_loops() {
        let src = "ScriptName T\nFunction F(Int i)\n    T before = Game.GetPlayer()\n    While i < 1\n        If i == 0\n            Game.GetPlayer()\n        EndIf\n    EndWhile\n    Game.GetPlayer()\nEndFunction\n";
        assert_eq!(check(src), ["6"]);
    }

    #[test]
    fn imported_and_in_the_condition_count_too() {
        let src = "ScriptName T\nImport Game\nFunction F()\n    While GetPlayer()\n    EndWhile\nEndFunction\n";
        assert_eq!(check(src), ["4"]);
    }

    #[test]
    fn functions_of_other_scripts_named_the_same_do_not() {
        let src = "ScriptName T\nFunction GetPlayer()\nEndFunction\nFunction F()\n    While True\n        GetPlayer()\n    EndWhile\nEndFunction\n";
        assert_eq!(check(src), [""; 0]);
    }
}
