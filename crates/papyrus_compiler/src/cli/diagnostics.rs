//! Printing the errors of a script: with its source lines for people, or
//! `file(line,col): message` like `PapyrusCompiler.exe`, for problem matchers.

use std::path::Path;

use annotate_snippets::{AnnotationKind, Level, Renderer, Snippet};
use papyrus_ide::{Diagnostic, Severity, Workspace};
/// A diagnostic with its source lines, and the related places (another
/// definition, the declaration of a script...) as context annotations.
pub(super) fn render(
    renderer: &Renderer,
    ws: &Workspace,
    path: &Path,
    text: &str,
    d: &Diagnostic,
) -> String {
    let level = match d.severity {
        Severity::Error => Level::ERROR,
        Severity::Warning => Level::WARNING,
    };
    let clamp = |text: &str, r: papyrus_ide::TextRange| {
        let start = usize::from(r.start()).min(text.len());
        start..usize::from(r.end()).clamp(start, text.len())
    };
    let path_text = path.display().to_string();
    let file = ws.file_id(path);
    // Related places of other files, with their text.
    let others: Vec<(String, std::sync::Arc<str>, &papyrus_ide::Related)> = d
        .related
        .iter()
        .filter(|r| Some(r.file) != file)
        .filter_map(|r| Some((ws.file(r.file).path.display().to_string(), ws.text(r.file)?, r)))
        .collect();
    let mut snippet = Snippet::source(text)
        .line_start(1)
        .path(&path_text)
        .annotation(AnnotationKind::Primary.span(clamp(text, d.range)).label(d.code));
    for r in d.related.iter().filter(|r| Some(r.file) == file) {
        snippet = snippet
            .annotation(AnnotationKind::Context.span(clamp(text, r.range)).label(&r.message));
    }
    let mut group = level.primary_title(&d.message).element(snippet);
    for (other_path, other_text, r) in &others {
        group = group.element(
            Snippet::source(&**other_text).line_start(1).path(other_path).annotation(
                AnnotationKind::Context.span(clamp(other_text, r.range)).label(&r.message),
            ),
        );
    }
    renderer.render(&[group])
}
/// `file(line,col): message`, one-based, as `PapyrusCompiler.exe` prints.
pub(super) fn render_official(path: &Path, text: &str, d: &Diagnostic) -> String {
    let start = usize::from(d.range.start()).min(text.len());
    let before = &text[..text.floor_char_boundary(start)];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    format!("{}({line},{col}): {}", path.display(), d.message)
}
