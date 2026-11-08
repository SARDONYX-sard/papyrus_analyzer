//! Quick fix of `script-name-mismatch`: renames the script after its file,
//! like a rename of the script would: in the other scripts using it too
//! (`extends`, types, calls), not only in its `ScriptName` line.
//!
//! In `MyQuest.psc`:
//!
//! ```papyrus
//! ScriptName MyQuets extends Quest    ; → ScriptName MyQuest extends Quest
//! ```
//!
//! and in `Other.psc`:
//!
//! ```papyrus
//! MyQuets Property Q Auto             ; → MyQuest Property Q Auto
//! ```

use super::{Assist, AssistCtx, AssistKind, TextEdit};
use crate::{
    FileId,
    features::{navigation::NavFile, rename::rename},
};

pub(crate) fn fix_script_name(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let index = ctx.sema.index;
    let (Some(name), Some(range)) = (index.name, index.name_range) else { return };
    if ctx.range.intersect(header_line(ctx, range)).is_none() {
        return;
    }
    let path = &ctx.sema.ws.file(ctx.sema.file).path;
    let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { return };
    if stem.eq_ignore_ascii_case(name.as_str()) {
        return;
    }
    let (edits, other_files) = rename_edits(ctx, range, &stem);
    acc.push(Assist {
        id: "fix_script_name",
        label: format!("Rename the script to `{stem}`, like its file"),
        kind: AssistKind::QuickFix,
        target: range,
        edits,
        fixes: Some("script-name-mismatch"),
        other_files,
    });
}

/// The edits renaming the script to `stem`: in this file, and in the others.
/// Only the `ScriptName` when the rename is refused (a name the game scripts
/// use, for one): the header is wrong anyway.
fn rename_edits(
    ctx: &AssistCtx<'_>,
    name: papyrus_parser::TextRange,
    stem: &str,
) -> (Vec<TextEdit>, Vec<(FileId, Vec<TextEdit>)>) {
    let this = ctx.sema.file;
    let only_header = || vec![TextEdit { range: name, new_text: stem.to_owned() }];
    let Ok(rename) = rename(ctx.sema.ws, this, name.start(), stem) else {
        return (only_header(), Vec::new());
    };
    let mut edits = Vec::new();
    let mut other_files: Vec<(FileId, Vec<TextEdit>)> = Vec::new();
    for (file, range) in rename.ranges {
        let NavFile::Script(file) = file else { continue };
        let edit = TextEdit { range, new_text: stem.to_owned() };
        if file == this {
            edits.push(edit);
        } else {
            // The ranges come sorted by file.
            match other_files.last_mut() {
                Some((f, e)) if *f == file => e.push(edit),
                _ => other_files.push((file, vec![edit])),
            }
        }
    }
    if !edits.iter().any(|e| e.range == name) {
        edits.extend(only_header());
    }
    (edits, other_files)
}

/// The `ScriptName` line, where the cursor may be.
fn header_line(ctx: &AssistCtx<'_>, name: papyrus_parser::TextRange) -> papyrus_parser::TextRange {
    super::line_range(ctx.sema.text, name)
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    use super::fix_script_name;
    use crate::{
        Workspace,
        assists::{
            AssistCtx,
            test_utils::{check_assist_in, check_assist_not_applicable},
        },
        features::with_sema,
    };

    #[test]
    fn renames_after_the_file() {
        check_assist_in(
            fix_script_name,
            "/ws/MyQuest.psc",
            "ScriptName My$0Quets extends Quest\n",
            "ScriptName MyQuest extends Quest\n",
        );
    }

    #[test]
    fn renames_the_uses_in_other_scripts() {
        let mut ws = Workspace::new();
        let quest = ws.set_open(
            PathBuf::from("/ws/MyQuest.psc"),
            Arc::from("ScriptName MyQuets\nFunction Go() Global\nEndFunction\n"),
            0,
        );
        let other = ws.set_open(
            PathBuf::from("/ws/Other.psc"),
            Arc::from(
                "ScriptName Other extends MyQuets\nMyQuets Property Q Auto\nFunction F()\n    MyQuets.Go()\nEndFunction\n",
            ),
            0,
        );
        let assists = with_sema(&ws, quest, |sema| {
            let mut acc = Vec::new();
            fix_script_name(&AssistCtx { sema, range: Default::default() }, &mut acc);
            acc
        })
        .unwrap();
        let assist = &assists[0];
        assert_eq!(assist.edits.len(), 1);
        let [(file, edits)] = assist.other_files.as_slice() else { panic!("{assist:?}") };
        assert_eq!(*file, other);
        assert_eq!(edits.len(), 3);
        assert!(edits.iter().all(|e| e.new_text == "MyQuest"));
    }

    #[test]
    fn not_when_matching() {
        // The test file is `T.psc`.
        check_assist_not_applicable(fix_script_name, "ScriptName $0t\n");
    }
}
