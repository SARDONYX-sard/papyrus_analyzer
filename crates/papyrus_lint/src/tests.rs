use std::{path::PathBuf, sync::Arc};

use papyrus_ide::{Severity, Workspace};

use crate::{Level, LintConfig, find, fix_all, lint};

#[test]
fn levels_by_name_group_and_all() {
    let unused = find("unused-local").unwrap();
    let bool_cmp = find("bool-literal-comparison").unwrap();
    let mut config = LintConfig::default();
    assert_eq!(config.level(unused), Level::Warn);

    config.set("all", Level::Allow).unwrap();
    assert_eq!(config.level(unused), Level::Allow);
    config.set("suspicious", Level::Deny).unwrap();
    assert_eq!(config.level(unused), Level::Deny);
    // A lint name wins over its group, whatever the order.
    config.set("unused-local", Level::Warn).unwrap();
    config.set("suspicious", Level::Allow).unwrap();
    assert_eq!(config.level(unused), Level::Warn);
    assert_eq!(config.level(bool_cmp), Level::Allow);

    // `-D warnings`
    config.set("warnings", Level::Deny).unwrap();
    assert_eq!(config.level(unused), Level::Deny);
    assert!(config.set("no-such-lint", Level::Deny).is_err());
}

#[test]
fn levels_set_severity_and_allow_silences() {
    let src = "ScriptName T\nFunction F(Bool b)\n    Int unused = 1\n    If b == True\n    EndIf\nEndFunction\n";
    let mut ws = Workspace::new();
    let file = ws.set_open(PathBuf::from("/ws/T.psc"), Arc::from(src), 0);
    let mut config = LintConfig::default();
    config.set("unused-local", Level::Deny).unwrap();
    config.set("complexity", Level::Allow).unwrap();
    let found: Vec<_> =
        lint(&ws, file, &config).into_iter().map(|d| (d.code, d.severity)).collect();
    assert_eq!(found, [("unused-local", Severity::Error)]);
}

#[test]
fn fix_all_applies_every_fix() {
    let src = "ScriptName T\nFunction F(Bool b)\n    Int unused = 1\n    If b == True && (b != False)\n    EndIf\nEndFunction\n";
    let mut ws = Workspace::new();
    let file = ws.set_open(PathBuf::from("/ws/T.psc"), Arc::from(src), 0);
    let (text, applied) = fix_all(&mut ws, file, &LintConfig::default(), false);
    assert_eq!(text, "ScriptName T\nFunction F(Bool b)\n    If b && (b)\n    EndIf\nEndFunction\n");
    assert_eq!(applied, 3);
}
