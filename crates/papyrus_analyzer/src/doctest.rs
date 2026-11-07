//! Doc tests, like rustdoc: the ```` ```papyrus ```` examples of documentation
//! comments are compiled and run by `papyrus_analyzer test`.
//!
//! ````papyrus
//! ; # Examples
//! ;
//! ; ```papyrus
//! ; # Int expected = 3
//! ; AssertIntEq(Add(1, 2), expected)
//! ; ```
//! Int Function Add(Int a, Int b) Global
//! {Adds.}
//! ````
//!
//! Examples go in `;` (or `;/ /;`) comments: `{ }` comments are compiled in the
//! `.pex` and shown in the Creation Kit, they are for the summary.
//!
//! - an example is a script when it starts with `ScriptName`; with declarations
//!   (`Function`, `Event`, `State`...), they go in a script extending the
//!   documented one, whose `OnInit` runs; else its statements run in a function of
//!   that script. `PapyrusAnalyzer` is imported: assertions work;
//! - it fails on a compile error, an error the game would log, a failed assertion;
//! - ```` ```papyrus,no_run ```` only compiles, ```` ```papyrus,ignore ```` is
//!   skipped;
//! - lines starting with `# ` are for the test only: hovers do not show them.

use std::path::{Path, PathBuf};

use papyrus_ide::Workspace;
use papyrus_vm::{Value, Vm};

/// What a doc test does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Compiles and runs.
    Run,
    /// Compiles only: `papyrus,no_run`.
    NoRun,
}

/// A `papyrus` example of documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Example {
    /// The code, its hidden lines shown.
    pub(crate) code: String,
    pub(crate) mode: Mode,
    /// The line of the opening fence in the Markdown, from 0.
    pub(crate) line: usize,
}

/// The `papyrus` examples of `markdown`, but those marked `ignore`.
pub(crate) fn examples(markdown: &str) -> Vec<Example> {
    let mut out = Vec::new();
    let mut open: Option<(Mode, usize, Vec<String>)> = None;
    let mut other = false;
    for (i, line) in markdown.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(info) = trimmed.strip_prefix("```") {
            if let Some((mode, at, lines)) = open.take() {
                out.push(Example { code: lines.join("\n") + "\n", mode, line: at });
            } else if other {
                other = false;
            } else {
                let mut attrs = info.split(',').map(|a| a.trim().to_ascii_lowercase());
                if attrs.next().as_deref() != Some("papyrus") {
                    other = true;
                    continue;
                }
                let attrs: Vec<String> = attrs.collect();
                if attrs.iter().any(|a| a == "ignore") {
                    // Skipped: its lines are text.
                    other = true;
                    continue;
                }
                let no_run = attrs.iter().any(|a| a == "no_run");
                open = Some((if no_run { Mode::NoRun } else { Mode::Run }, i, Vec::new()));
            }
            continue;
        }
        if let Some((_, _, lines)) = &mut open {
            let shown =
                if trimmed == "#" { "" } else { trimmed.strip_prefix("# ").unwrap_or(line) };
            lines.push(shown.to_owned());
        }
    }
    out
}

/// How an example runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prepared {
    /// The script to compile.
    pub(crate) script: String,
    pub(crate) source: String,
    /// The function running its statements; `None`: `OnInit` only.
    pub(crate) run: Option<&'static str>,
    /// A whole script: compiled only, nothing runs it.
    pub(crate) compile_only: bool,
}

const RUN: &str = "DocTestRun";

/// Whether a line of an example declares something of a script.
fn is_declaration(line: &str) -> bool {
    if line.starts_with([' ', '\t']) {
        return false;
    }
    let lower = line.to_ascii_lowercase();
    let starts = ["function ", "event ", "state ", "auto state ", "import ", "scriptname "];
    starts.iter().any(|s| lower.starts_with(s))
        || lower.contains(" function ")
        || lower.contains(" event ")
        || lower.contains(" property ")
}

/// The script of an example: `name` extending `base`.
pub(crate) fn prepare(example: &Example, name: &str, base: Option<&str>) -> Prepared {
    let first = example
        .code
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with(';'))
        .unwrap_or_default();
    if first.to_ascii_lowercase().starts_with("scriptname ") {
        let script = first.split_whitespace().nth(1).unwrap_or(name).to_owned();
        return Prepared { script, source: example.code.clone(), run: None, compile_only: true };
    }
    let extends = base.map(|b| format!(" extends {b}")).unwrap_or_default();
    let header = format!("ScriptName {name}{extends}\nImport PapyrusAnalyzer\n");
    if example.code.lines().any(is_declaration) {
        let source = format!("{header}{}", example.code);
        return Prepared { script: name.to_owned(), source, run: None, compile_only: false };
    }
    let body: String = example.code.lines().map(|l| format!("    {l}\n")).collect();
    let source = format!("{header}Function {RUN}()\n{body}EndFunction\n");
    Prepared { script: name.to_owned(), source, run: Some(RUN), compile_only: false }
}

/// Compiles the example in a copy of `ws`, the workspace of its scripts, then runs
/// it; the problem if it fails.
pub(crate) fn run(
    ws: &Workspace,
    imports: &[PathBuf],
    dir: &Path,
    example: &Example,
    name: &str,
    base: Option<&str>,
) -> Result<(), String> {
    let prepared = prepare(example, name, base);
    let mut ws = ws.clone();
    let path = dir.join(format!("{}.psc", prepared.script));
    // The scripts it uses, from its text: it is not on disk.
    papyrus_compiler::loader::load_texts(
        &mut ws,
        &[(path.clone(), prepared.source.clone())],
        imports,
    );
    let file = ws.set_open(path, std::sync::Arc::from(prepared.source.as_str()), 0);
    if let Err(e) = papyrus_compiler::compile(&ws, file, &Default::default()) {
        return Err(match e {
            papyrus_compiler::CompileError::Diagnostics(errors) => {
                let lines: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
                format!("does not compile: {}\n{}", lines.join("; "), prepared.source)
            }
            papyrus_compiler::CompileError::Unreadable => "cannot read the example".to_owned(),
        });
    }
    if example.mode == Mode::NoRun || prepared.compile_only {
        return Ok(());
    }
    let mut vm = Vm::new(crate::run::Compiled::new(ws, imports.to_vec()));
    let result = vm.new_object(&prepared.script).and_then(|obj| {
        vm.send_event(obj, "OnInit", &[])?;
        match prepared.run {
            Some(function) => vm.call(obj, function, &[]),
            None => Ok(Value::None),
        }
    });
    crate::run::problem(&vm, &result).map_or(Ok(()), |p| {
        let output = vm.output.join("\n");
        Err(if output.is_empty() { p } else { format!("{output}\n{p}") })
    })
}

/// A doc test found in the scripts: its name, the example and the script it
/// documents.
pub(crate) struct DocTest {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    /// The line of the opening fence in the script, from 1.
    pub(crate) line: usize,
    pub(crate) example: Example,
    pub(crate) base: Option<String>,
}

/// The doc tests of the documentation comments of `scripts`.
pub(crate) fn find(scripts: &[PathBuf]) -> Vec<DocTest> {
    let read = scripts.iter().filter_map(|p| Some((p, papyrus_ide::read_file(p)?)));
    read.flat_map(|(path, text)| find_in(path, &text)).collect()
}

/// The doc tests of the Markdown files `paths`: their examples, in scripts of
/// their own. Named after their path from `root`, like `README.md (line 5)`.
pub(crate) fn find_markdown(paths: &[PathBuf], root: &Path) -> Vec<DocTest> {
    let mut out = Vec::new();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let shown =
            path.strip_prefix(root).unwrap_or(path).display().to_string().replace('\\', "/");
        for example in examples(&text.replace("\r\n", "\n")) {
            let line = example.line + 1;
            out.push(DocTest {
                name: format!("{shown} (line {line})"),
                path: path.clone(),
                line,
                example,
                base: None,
            });
        }
    }
    out
}

/// The doc tests of the script `path`, whose text is `text`.
pub(crate) fn find_in(path: &Path, text: &str) -> Vec<DocTest> {
    let mut out = Vec::new();
    if !text.contains("```") {
        return out;
    }
    let index = papyrus_ide::index_text(text);
    let Some(script) = index.name.map(|n| n.to_string()) else { return out };
    let documented = std::iter::once((script.clone(), &index.docs[..]))
        .chain(index.members.iter().map(|m| (format!("{script}.{}", m.name), &m.docs[..])));
    for (what, ranges) in documented {
        let lines = papyrus_ide::index::doc_comment_lines(text, ranges);
        let markdown: Vec<&str> = lines.iter().map(|(_, l)| l.as_str()).collect();
        for example in examples(&markdown.join("\n")) {
            let line = lines[example.line].0 + 1;
            out.push(DocTest {
                // Like `cargo test`: `src/lib.rs - foo (line 5)`.
                name: format!("{what} (line {line})"),
                path: path.to_path_buf(),
                line,
                example,
                base: Some(script.clone()),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_examples_to_test() {
        let md = "Text.\n\n```papyrus\n# Int hidden = 1\nDebug.Trace(hidden)\n```\n\n```papyrus,no_run\nGame.QuitToMainMenu()\n```\n\n```papyrus,ignore\nNotEven Papyrus\n```\n\n```toml\n[build]\n```\n";
        let found = examples(md);
        assert_eq!(
            found,
            [
                Example {
                    code: "Int hidden = 1\nDebug.Trace(hidden)\n".into(),
                    mode: Mode::Run,
                    line: 2
                },
                Example { code: "Game.QuitToMainMenu()\n".into(), mode: Mode::NoRun, line: 7 },
            ]
        );
        // What readers see.
        let shown = papyrus_ide::index::hide_test_lines(md);
        assert!(!shown.contains("hidden = 1") && shown.contains("Debug.Trace(hidden)"), "{shown}");
        assert!(
            !shown.contains("papyrus,no_run") && shown.contains("```papyrus\nGame."),
            "{shown}"
        );
    }

    #[test]
    fn doc_tests_are_named_after_the_line_of_their_fence() {
        let text = "ScriptName T\nFunction F() Global\n{Text.\n\n```papyrus\nF()\n```}\nEndFunction\nFunction G() Global\n{\n  Text.\n\n  ```papyrus\n  G()\n  ```\n}\nEndFunction\n\n; Text.\n;\n; ```papyrus\n; If True\n;     H()\n; EndIf\n; ```\nFunction H() Global\n{Shown in the Creation Kit.}\nEndFunction\n;/\nText.\n\n```papyrus\nI()\n```\n/;\nFunction I() Global\nEndFunction\n";
        let found = find_in(Path::new("/ws/T.psc"), text);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(found.len(), 4);
        for doc in &found {
            assert_eq!(
                lines[doc.line - 1].trim_start_matches(';').trim(),
                "```papyrus",
                "{}",
                doc.name
            );
        }
        let names: Vec<&str> = found.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["T.F (line 5)", "T.G (line 13)", "T.H (line 21)", "T.I (line 32)"]);
        // `; ` is removed, the indentation of the code is kept.
        assert_eq!(found[2].example.code, "If True\n    H()\nEndIf\n");
    }

    #[test]
    fn wraps_the_examples() {
        let ex = |code: &str| Example { code: code.into(), mode: Mode::Run, line: 0 };
        let statements = prepare(&ex("Int a = 1\n"), "DocTest", Some("Base"));
        assert_eq!(statements.run, Some(RUN));
        assert!(statements.source.starts_with("ScriptName DocTest extends Base\nImport PapyrusAnalyzer\nFunction DocTestRun()\n    Int a = 1\n"));
        let items =
            prepare(&ex("Int Property P Auto\nEvent OnInit()\nEndEvent\n"), "DocTest", None);
        assert_eq!((items.run, items.compile_only), (None, false));
        let script = prepare(&ex("; a script\nScriptName Mine extends Actor\n"), "DocTest", None);
        assert_eq!((script.script.as_str(), script.compile_only), ("Mine", true));
    }

    /// The scripts of the game, from the corpus, for the examples of the
    /// documentation of papyrus_analyzer.
    fn game_scripts() -> Option<PathBuf> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/psc_deps");
        dir.join("Actor.psc").is_file().then_some(dir)
    }

    /// Runs the examples of `markdown`, documenting `what`.
    fn check(failures: &mut Vec<String>, docs: &[DocTest], game: &Path) {
        let dir = std::env::temp_dir().join("papyrus-doc-tests");
        let mut ws = Workspace::new();
        crate::builtin::install(&mut ws, std::slice::from_ref(&dir));
        for (i, doc) in docs.iter().enumerate() {
            let name = format!("DocTest{i}");
            let base = doc.base.as_deref();
            if let Err(problem) = run(&ws, &[game.to_path_buf()], &dir, &doc.example, &name, base) {
                failures.push(format!("{}: {problem}", doc.name));
            }
        }
    }

    #[test]
    fn the_documentation_of_papyrus_analyzer_works() {
        let Some(game) = game_scripts() else {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("skipped: no tests/corpus/psc_deps");
            }
            return;
        };
        // Like `papyrus_analyzer test --doc` does: the Markdown files in scripts
        // of their own, the built-in script of tests as its users read it.
        let mut docs = Vec::new();
        for (what, markdown) in papyrus_ide::documentation() {
            docs.extend(examples(&markdown.replace("\r\n", "\n")).into_iter().map(|example| {
                let line = example.line + 1;
                DocTest {
                    name: format!("{what} (line {line})"),
                    path: PathBuf::new(),
                    line,
                    example,
                    base: None,
                }
            }));
        }
        let psc = papyrus_vm::PAPYRUS_ANALYZER_PSC.replace("\r\n", "\n");
        docs.extend(find_in(Path::new("PapyrusAnalyzer.psc"), &psc));
        assert!(docs.len() > 30);
        let mut failures = Vec::new();
        check(&mut failures, &docs, &game);
        assert!(failures.is_empty(), "{} failed:\n\n{}", failures.len(), failures.join("\n\n"));
    }
}
