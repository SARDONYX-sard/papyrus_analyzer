//! `script-name-case`: a `ScriptName` spelled like its file but for the case.
//!
//! In `MEP_CageManager.psc`:
//!
//! ```papyrus
//! ScriptName mep_cagemanager extends Quest   ; → ScriptName MEP_CageManager
//! ```
//!
//! Papyrus ignores case, but Creation Kit may not: the formatter writes the
//! uses of a script like its `ScriptName` (`canonical`), so the property types
//! of the other scripts would follow `mep_cagemanager` instead of the file.
//!
//! Fix: renames the script after its file, in the other scripts too. Manual:
//! renaming the file instead may be the right fix.

use papyrus_ide::{Diagnostic, assists::fix_script_name_case, sema::Sema};

use crate::{Group, Lint};

const CODE: &str = "script-name-case";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Style,
    summary: "a script name spelled with another case than its file",
    run,
    fix: Some(fix_script_name_case),
    manual_fix: true,
};

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    let Some(range) = sema.index.name_range else { return };
    let name = &sema.text[range];
    let path = &sema.ws.file(sema.file).path;
    let Some(stem) = path.file_stem().map(|s| s.to_string_lossy()) else { return };
    // Other names are `script-name-mismatch`.
    if stem != name && stem.eq_ignore_ascii_case(name) {
        out.push(Diagnostic::warning(
            range,
            CODE,
            format!("script name `{name}` is spelled `{stem}` in the file name `{stem}.psc`"),
        ));
    }
}

#[cfg(test)]
mod tests {
    use crate::test_utils::check;

    // The test file is `T.psc`.
    #[test]
    fn reports_another_case() {
        assert_eq!(
            check("script-name-case", "ScriptName t\n"),
            ["1: script name `t` is spelled `T` in the file name `T.psc`"]
        );
    }

    #[test]
    fn same_spelling_and_other_names_are_not_reported() {
        assert_eq!(check("script-name-case", "ScriptName T\n"), [""; 0]);
        assert_eq!(check("script-name-case", "ScriptName Other\n"), [""; 0]);
    }
}
