//! `name-case`: a name spelled otherwise than its case setting, by default
//! like its definition.
//!
//! ```papyrus
//! actor Property PlayerRef Auto      ; → Actor (its ScriptName)
//! Function F()
//!     playerref.getactorbase()        ; → PlayerRef.GetActorBase()
//! EndFunction
//! ```
//!
//! Papyrus ignores case, so these work; the lint keeps the scripts of the
//! workspace spelled alike. Each kind of name has its case, set in the table of
//! the lint:
//!
//! ```toml
//! [lints]
//! name-case = { level = "warn", script = "canonical", function = "PascalCase", variable = "preserve" }
//! ```
//!
//! `canonical` is the definition (the `ScriptName`, the declaration, the
//! function of the parent script for overrides, the flags file), which the
//! other cases start from; `preserve` turns the lint off for that kind.
//!
//! Only the scripts of the workspace are checked; imported scripts (the game,
//! other mods) are not ours. The names they define are still checked where our
//! scripts use them (`actor` → `Actor`).
//!
//! This used to be done by the formatter, but Creation Kit may bind script and
//! property names with case (#4): changing them is up to the user.
//!
//! Fix: respells the name.

use papyrus_fmt::Case;
use papyrus_ide::{
    Diagnostic, FileId, NameKind, TextRange, Workspace,
    assists::{Assist, AssistKind, TextEdit},
    sema::Sema,
};

use crate::{Group, Lint, LintConfig};

pub(crate) const CODE: &str = "name-case";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Pedantic,
    summary: "a name spelled otherwise than its definition (or the `name-case` setting)",
    run: |_, _| {},
    fix: None,
    manual_fix: false,
};

/// The case of each kind of name, see the module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NameCases {
    /// Scripts: types, `extends`, `Import`, receivers like `Debug.Trace`.
    pub script: Case,
    /// Functions and events.
    pub function: Case,
    /// Properties.
    pub property: Case,
    /// Local variables, parameters and script variables.
    pub variable: Case,
    /// User flags such as `Hidden`.
    pub flag: Case,
}

impl Default for NameCases {
    fn default() -> Self {
        let c = Case::Canonical;
        NameCases { script: c, function: c, property: c, variable: c, flag: c }
    }
}

impl NameCases {
    /// The keys of the table of the lint, besides `level`.
    pub const KEYS: [&'static str; 5] = ["script", "function", "property", "variable", "flag"];

    /// Sets the case of the kind `key`, one of [`NameCases::KEYS`].
    pub fn set(&mut self, key: &str, case: &str) -> Result<(), String> {
        let case: Case = case.parse()?;
        let slot = match key {
            "script" => &mut self.script,
            "function" => &mut self.function,
            "property" => &mut self.property,
            "variable" => &mut self.variable,
            "flag" => &mut self.flag,
            _ => {
                return Err(format!(
                    "unknown key `{key}` of `name-case`: expected `level`, {}",
                    Self::KEYS.map(|k| format!("`{k}`")).join(", ")
                ));
            }
        };
        *slot = case;
        Ok(())
    }

    fn of(&self, kind: NameKind) -> Case {
        match kind {
            NameKind::Script => self.script,
            NameKind::Function => self.function,
            NameKind::Property => self.property,
            NameKind::Variable => self.variable,
            NameKind::Flag => self.flag,
        }
    }
}

/// The names of `file` spelled otherwise than `cases` want: their range and
/// the wanted spelling.
fn misspelled(
    ws: &Workspace,
    file: FileId,
    text: &str,
    cases: &NameCases,
) -> Vec<(TextRange, String)> {
    if !ws.file(file).origin.is_own() {
        return Vec::new();
    }
    papyrus_ide::name_spellings(ws, file)
        .into_iter()
        .filter_map(|n| {
            let written = &text[n.range];
            let wanted = cases.of(n.kind).name_spelling(&n.spelling, written);
            (wanted != written).then(|| (n.range, wanted.into_owned()))
        })
        .collect()
}

pub(crate) fn run(sema: &Sema<'_>, config: &LintConfig, out: &mut Vec<Diagnostic>) {
    for (range, wanted) in misspelled(sema.ws, sema.file, sema.text, &config.name_cases) {
        let message = format!("`{}` is spelled `{wanted}`", &sema.text[range]);
        out.push(Diagnostic::warning(range, CODE, message));
    }
}

pub(crate) fn fixes(
    ws: &Workspace,
    file: FileId,
    range: TextRange,
    config: &LintConfig,
    acc: &mut Vec<Assist>,
) {
    let Some(text) = ws.text(file) else { return };
    for (target, wanted) in misspelled(ws, file, &text, &config.name_cases) {
        if target.intersect(range).is_none() {
            continue;
        }
        acc.push(Assist {
            id: "fix_name_case",
            label: format!("Spell `{wanted}`"),
            kind: AssistKind::QuickFix,
            target,
            edits: vec![TextEdit { range: target, new_text: wanted }],
            fixes: Some(CODE),
            other_files: Vec::new(),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    use papyrus_fmt::Case;
    use papyrus_ide::{Origin, Workspace};

    use crate::{Level, LintConfig, fix_all, lint};

    fn on() -> LintConfig {
        let mut config = LintConfig::default();
        config.set("name-case", Level::Warn).unwrap();
        config
    }

    fn check(src: &str) -> Vec<String> {
        let mut ws = Workspace::new();
        let file = ws.set_open(PathBuf::from("/ws/T.psc"), Arc::from(src), 0);
        lint(&ws, file, &on())
            .into_iter()
            .filter(|d| d.code == "name-case")
            .map(|d| {
                let line = src[..usize::from(d.range.start())].matches('\n').count() + 1;
                format!("{line}: {}", d.message)
            })
            .collect()
    }

    const SRC: &str = "ScriptName T\nInt Property Count Auto\nFunction GetCount()\nEndFunction\nFunction F(Int value)\n    count = VALUE\n    getcount()\n    t.getCOUNT()\nEndFunction\n";

    #[test]
    fn reports_names_spelled_otherwise_than_their_definition() {
        assert_eq!(
            check(SRC),
            [
                "6: `count` is spelled `Count`",
                "6: `VALUE` is spelled `value`",
                "7: `getcount` is spelled `GetCount`",
                "8: `t` is spelled `T`",
                "8: `getCOUNT` is spelled `GetCount`",
            ]
        );
    }

    #[test]
    fn each_kind_has_its_case() {
        let mut config = on();
        config.name_cases.variable = Case::Preserve;
        config.name_cases.function = Case::Uppercase;
        let mut ws = Workspace::new();
        let file = ws.set_open(PathBuf::from("/ws/T.psc"), Arc::from(SRC), 0);
        let found: Vec<String> = lint(&ws, file, &config)
            .into_iter()
            .filter(|d| d.code == "name-case")
            .map(|d| d.message)
            .collect();
        assert_eq!(
            found,
            [
                // The declaration too: the case starts from it.
                "`GetCount` is spelled `GETCOUNT`",
                "`count` is spelled `Count`",
                "`getcount` is spelled `GETCOUNT`",
                "`t` is spelled `T`",
                "`getCOUNT` is spelled `GETCOUNT`",
            ]
        );
    }

    #[test]
    fn fix_respells() {
        let mut ws = Workspace::new();
        let file = ws.set_open(PathBuf::from("/ws/T.psc"), Arc::from(SRC), 0);
        let (text, _) = fix_all(&mut ws, file, &on(), false);
        assert!(text.contains("    Count = value\n    GetCount()\n    T.GetCount()\n"), "{text}");
    }

    #[test]
    fn imported_scripts_are_not_checked() {
        let mut ws = Workspace::new();
        let src = "ScriptName Lib\nFunction Go() Global\n    go()\nEndFunction\n";
        let file = ws.set_open(PathBuf::from("/game/Lib.psc"), Arc::from(src), 0);
        ws.set_origin(file, Origin::Import(0));
        assert!(lint(&ws, file, &on()).iter().all(|d| d.code != "name-case"));
    }
}
