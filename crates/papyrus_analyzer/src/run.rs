//! `papyrus_analyzer run Script.Function [ARG]...`: runs a function in the
//! Papyrus virtual machine of [`papyrus_vm`], outside of the game.
//!
//! The scripts below the current directory and the imports of its
//! `papyrus.toml` (else the game scripts) are compiled in memory with the
//! built-in compiler, when the run needs them. Natives without implementation
//! return their default value; every native call is listed at the end.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::{
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex, OnceLock},
};

use papyrus_ide::Workspace;
use papyrus_pex::PexFile;
use papyrus_vm::{Script, ScriptLoader, Value, Vm};
use rayon::prelude::*;
use rustc_hash::FxHashMap;

const USAGE: &str = "\
usage: papyrus_analyzer run Script.Function [ARG]...

  ARG   Int (`3`), Float (`1.5`), Bool (`True`), None, or a String

Runs the function on a new object of Script, after its `OnInit`.
Debug.Trace prints; natives without implementation return their default
value (0, None, \"\"...) and are listed at the end. `Utility.Wait` sends
the `OnUpdate` events registered with `RegisterForUpdate`.";

const TEST_USAGE: &str = "\
usage: papyrus_analyzer test [PATH | FILTER]...

Runs the `; #[test]` functions of the scripts below the current directory,
each on a new object of its script, after its `OnInit`. A test fails when a
`PapyrusAnalyzer.Assert*` fails, when the game would log an error (a call on
None...) or when it returns False. `; #[mock(Script.Function)]` functions of
the test script replace the native in its tests.

The scripts of the [test] sources of papyrus.toml see the built-in script
`PapyrusAnalyzer` (`AssertIntEq`, `NewObject`, `AssertCalled`...; `Import
PapyrusAnalyzer` calls them unqualified). A project script of that name wins.

Then the doc tests, like `cargo test`: the ```papyrus examples of the
documentation comments compile and run (`papyrus,no_run`: compile only,
`papyrus,ignore`: skipped; lines starting with `# ` are hidden from hovers).

  PATH     only the tests of the scripts of this file or directory
  FILTER   only the tests whose `Script.Function` contains it (any case)

options:
  --doc        only the doc tests
  -h, --help   print this help";

/// Compiles the scripts of the workspace when the machine loads them. Cheap to
/// clone: each thread running tests has its own machine, and its own copy;
/// the copies share what they compiled.
#[derive(Clone)]
pub(crate) struct Compiled {
    ws: Workspace,
    imports: Vec<PathBuf>,
    compiled: Arc<PexCache>,
}

/// The scripts compiled, by lowercase name; `None` when they do not compile.
/// A script is compiled once: a thread asking for it while another compiles
/// it waits for that one.
type PexCache = Mutex<FxHashMap<String, Arc<OnceLock<Option<Arc<PexFile<'static>>>>>>>;

impl Compiled {
    /// Compiles the scripts of `ws`, whose scripts use those of `imports`.
    pub(crate) fn new(ws: Workspace, imports: Vec<PathBuf>) -> Compiled {
        Compiled { ws, imports, compiled: Arc::default() }
    }

    /// Compiles the script `name` with the workspace of this thread.
    fn compile(&mut self, name: &str) -> Option<PexFile<'static>> {
        let find = |ws: &Workspace| {
            ws.files()
                .find(|(_, d)| d.index.name.is_some_and(|n| n.as_str().eq_ignore_ascii_case(name)))
                .map(|(f, d)| (f, d.path.clone()))
        };
        // The scripts its code uses, which the first load may have skipped
        // (only the declarations of the scripts the targets use are read).
        let (_, path) = find(&self.ws)?;
        papyrus_compiler::loader::load(&mut self.ws, &[path], &self.imports);
        let (file, _) = find(&self.ws)?;
        match papyrus_compiler::compile(&self.ws, file, &Default::default()) {
            Ok(pex) => Some(pex),
            Err(e) => {
                eprintln!("cannot compile {name}: {e:?}");
                None
            }
        }
    }

    /// The files of the scripts, by lowercase name: where errors happened.
    fn script_paths(&self) -> ScriptPaths {
        self.ws
            .files()
            .filter_map(|(_, d)| {
                Some((d.index.name?.as_str().to_ascii_lowercase(), d.path.clone()))
            })
            .collect()
    }
}

/// The file of each script, by lowercase name.
type ScriptPaths = FxHashMap<String, PathBuf>;

/// The errors of the run that happened in a script file, as
/// `path(line,column): message`: the format of the compiler errors, which the
/// problem matchers of editors turn into diagnostics at that place. The column
/// is the start of the line.
fn located_errors(vm: &Vm, paths: &ScriptPaths) -> Vec<String> {
    (0..vm.errors.len())
        .filter_map(|i| {
            let site = vm.error_site(i)?;
            let line = site.line?;
            let path = paths.get(&site.script.to_ascii_lowercase())?;
            let text = papyrus_ide::read_file(path).unwrap_or_default();
            let indent = text
                .lines()
                .nth(line.saturating_sub(1) as usize)
                .map_or(0, |l| l.chars().take_while(|c| c.is_whitespace()).count());
            Some(format!("{}({line},{}): {}", path.display(), indent + 1, vm.errors[i]))
        })
        .collect()
}

impl ScriptLoader for Compiled {
    fn load(&mut self, name: &str) -> Option<Script> {
        let cell = {
            let mut compiled = self.compiled.lock().unwrap_or_else(|e| e.into_inner());
            compiled.entry(name.to_ascii_lowercase()).or_default().clone()
        };
        let pex = cell.get_or_init(|| self.compile(name).map(Arc::new)).clone()?;
        Script::from_pex(&pex)
    }
}

/// The loader of the scripts below the current directory, their imports and
/// the built-in `PapyrusAnalyzer`, which every script of the run sees.
pub(crate) fn loader(scripts: &[PathBuf]) -> Compiled {
    let (mut ws, imports) = crate::fmt::workspace_with_imports(scripts);
    let cwd = std::env::current_dir().unwrap_or_default();
    crate::builtin::install(&mut ws, &[cwd]);
    if let Some(path) = crate::builtin::shadowed_by(&ws) {
        eprintln!(
            "warning: {} hides the built-in `{}`",
            path.display(),
            papyrus_vm::PAPYRUS_ANALYZER
        );
    }
    Compiled::new(ws, imports)
}

/// An argument from the command line.
pub(crate) fn parse_arg(arg: &str) -> Value {
    if let Ok(i) = arg.parse() {
        return Value::Int(i);
    }
    if let Ok(f) = arg.parse() {
        return Value::Float(f);
    }
    match arg.to_ascii_lowercase().as_str() {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "none" => Value::None,
        _ => Value::Str(Rc::from(arg)),
    }
}

/// Runs the subcommand and returns the process exit code.
pub(crate) fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    let Some((script, function)) = args.first().and_then(|a| a.split_once('.')) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let values: Vec<Value> = args[1..].iter().map(|a| parse_arg(a)).collect();

    let mut scripts: Vec<PathBuf> = Vec::new();
    collect(&std::env::current_dir().unwrap_or_default(), &mut scripts);
    let started = std::time::Instant::now();
    let compiled = loader(&scripts);
    let paths = compiled.script_paths();
    let mut vm = Vm::new(compiled);
    let marked = marked_functions(&scripts);
    let result = start(&mut vm, &marked, script).and_then(|obj| vm.call(obj, function, &values));

    // Like `cargo test` with one test: sections separated by blank lines.
    let name = format!("{script}.{function}");
    println!("\nrunning 1 test\n\n---- {name} stdout ----");
    for line in &vm.output {
        println!("{line}");
    }
    let mut errors = vm.errors.clone();
    if let Err(e) = &result {
        errors.push(e.to_string());
    }
    if !errors.is_empty() {
        println!("\nerrors:");
        for e in &errors {
            println!("    {e}");
        }
        // At the start of the lines, for the problem matchers.
        for at in located_errors(&vm, &paths) {
            println!("{at}");
        }
    }
    if !vm.native_calls.is_empty() {
        println!("\nnative calls:");
        let show = |v: &Value| match v {
            Value::Str(s) => format!("{s:?}"),
            other => vm.text(other),
        };
        for c in &vm.native_calls {
            let args: Vec<String> = c.args.iter().map(show).collect();
            let mark = if c.implemented { "" } else { "  (not implemented: default value)" };
            println!("    {}.{}({}){mark}", c.script, c.function, args.join(", "));
        }
    }
    if let Ok(v) = &result
        && !matches!(v, Value::None)
    {
        println!("\n=> {}", vm.text(v));
    }
    // Like `test`: errors or `False` fail, the exit code says so.
    let failed = problem(&vm, &result).is_some();
    let (verdict, title) =
        if failed { (paint(RED, "FAILED"), "failures") } else { (paint(GREEN, "ok"), "successes") };
    println!("\n{title}:\n    {name}");
    // The summary of `test`, for one test.
    println!(
        "\ntest result: {verdict}. {} passed; {} failed; 0 filtered out; finished in {:.2}s\n",
        u8::from(!failed),
        u8::from(failed),
        started.elapsed().as_secs_f64()
    );
    i32::from(failed)
}

/// `papyrus_analyzer test`: runs the functions marked `; #[test]` of the
/// scripts below the current directory. A test fails when the machine
/// reports an error (a call on `None`...) or when it returns `False`:
///
/// ```papyrus
/// ; #[test]
/// Bool Function AddNumbersWorks()
///     Return AddNumbers(1, 2) == 3
/// EndFunction
/// ```
pub(crate) fn test(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{TEST_USAGE}");
        return 0;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut scripts: Vec<PathBuf> = Vec::new();
    collect(&cwd, &mut scripts);
    let doc_only = args.iter().any(|a| a == "--doc");
    let mut failed = false;

    if !doc_only {
        let started = std::time::Instant::now();
        let marked = marked_functions(&scripts);
        let all = marked.iter().filter(|m| m.mocks.is_none());
        let tests: Vec<&Marked> = all
            .clone()
            .filter(|m| selected(&format!("{}.{}", m.script, m.function), &m.path, args))
            .collect();
        let filtered_out = all.count() - tests.len();

        // Like `cargo test`: a line per test, then the output of the failures,
        // then the summary.
        let plural = if tests.len() == 1 { "" } else { "s" };
        println!("\nrunning {} test{plural}", tests.len());
        let compiled = loader(&scripts);
        let paths = compiled.script_paths();
        // Tests are independent: they run on all cores, each with a machine of
        // its thread (the machines share what they compiled). `collect` keeps
        // the order.
        let results: Vec<TestResult> = tests
            .par_iter()
            .map_init(
                || Vm::new(compiled.clone()),
                |vm, &Marked { script, function, .. }| {
                    vm.begin_test();
                    let result =
                        start(vm, &marked, script).and_then(|obj| vm.call(obj, function, &[]));
                    let problem = match &result {
                        // One line per error, like a panic message.
                        Ok(_) if !vm.errors.is_empty() => Some(vm.errors.join("\n")),
                        _ => problem(vm, &result),
                    };
                    let name = format!("{script}.{function}");
                    let located = located_errors(vm, &paths);
                    (name, std::mem::take(&mut vm.output), problem, located)
                },
            )
            .collect();
        for (name, _, problem, _) in &results {
            print_verdict(name, problem.is_some());
        }
        failed |= report(&results, filtered_out, started);
    }

    // Like `cargo test`, the doc tests after: of the scripts, then of the
    // Markdown files (a README...).
    let mut markdown = Vec::new();
    collect_ext(&cwd, "md", &mut markdown);
    let mut docs = crate::doctest::find(&scripts);
    docs.extend(crate::doctest::find_markdown(&markdown, &cwd));
    if doc_only || !docs.is_empty() {
        let started = std::time::Instant::now();
        let all = docs.len();
        let docs: Vec<_> = docs.into_iter().filter(|d| selected(&d.name, &d.path, args)).collect();
        println!("   Doc-tests\n");
        let plural = if docs.len() == 1 { "" } else { "s" };
        println!("running {} test{plural}", docs.len());
        let Compiled { ws, imports, .. } = loader(&scripts);
        // Where the scripts of the examples are, in memory only: below the test
        // directory, they see `PapyrusAnalyzer`.
        let dir = cwd.join(".papyrus-doc-tests");
        let results: Vec<TestResult> = docs
            .par_iter()
            .enumerate()
            .map(|(i, doc)| {
                let name = format!("DocTest{i}");
                let base = doc.base.as_deref();
                let problem =
                    crate::doctest::run(&ws, &imports, &dir, &doc.example, &name, base).err();
                (doc.name.clone(), Vec::new(), problem, Vec::new())
            })
            .collect();
        for (name, _, problem, _) in &results {
            print_verdict(name, problem.is_some());
        }
        failed |= report(&results, all - docs.len(), started);
    }
    i32::from(failed)
}

/// A test: its name, its output (`Debug.Trace`...), why it failed, and where
/// (see [`located_errors`]).
type TestResult = (String, Vec<String>, Option<String>, Vec<String>);

fn print_verdict(name: &str, failed: bool) {
    let verdict = if failed { paint(RED, "FAILED") } else { paint(GREEN, "ok") };
    println!("test {name} ... {verdict}");
}

/// Like `cargo test`: the output of the failures, then the summary. Whether a
/// test failed.
fn report(results: &[TestResult], filtered_out: usize, started: std::time::Instant) -> bool {
    let failures: Vec<_> = results
        .iter()
        .filter_map(|(name, output, problem, located)| {
            Some((name, output, problem.as_ref()?, located))
        })
        .collect();
    if !failures.is_empty() {
        println!("\nfailures:\n");
        for (name, output, problem, located) in &failures {
            println!("---- {name} stdout ----");
            for line in output.iter() {
                println!("{line}");
            }
            println!("{problem}");
            // At the start of the lines, for the problem matchers.
            for at in located.iter() {
                println!("{at}");
            }
            println!();
        }
        println!("failures:");
        for (name, ..) in &failures {
            println!("    {name}");
        }
    }

    let failed = results.iter().filter(|(_, _, p, _)| p.is_some()).count();
    let verdict = if failed > 0 { paint(RED, "FAILED") } else { paint(GREEN, "ok") };
    println!(
        "\ntest result: {verdict}. {} passed; {failed} failed; {filtered_out} filtered out; finished in {:.2}s\n",
        results.len() - failed,
        started.elapsed().as_secs_f64()
    );
    failed > 0
}

const GREEN: &str = "32";
const RED: &str = "31";

/// `text` in the color `sgr` (an ANSI code) when stdout is a terminal and
/// `NO_COLOR` is not set.
fn paint(sgr: &str, text: &str) -> String {
    use std::io::IsTerminal;
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    if color { format!("\x1b[{sgr}m{text}\x1b[0m") } else { text.to_owned() }
}

/// Whether the arguments of `test` select the test `name` of the file `path`:
/// every test without any; else the tests below a path argument, or whose name
/// contains another one.
fn selected(name: &str, path: &std::path::Path, args: &[String]) -> bool {
    let name = name.to_ascii_lowercase();
    let mut filters = args.iter().filter(|a| !a.starts_with('-')).peekable();
    if filters.peek().is_none() {
        return true;
    }
    let below = |dir: &std::path::Path| {
        let canonical =
            |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
        canonical(path).starts_with(canonical(dir))
    };
    filters.any(|f| {
        let path = std::path::Path::new(f);
        if path.exists() { below(path) } else { name.contains(&f.to_ascii_lowercase()) }
    })
}

/// Why a test failed: an error of the machine, errors the game would log
/// (failed assertions among them), or `False` returned.
pub(crate) fn problem(vm: &Vm, result: &Result<Value, papyrus_vm::VmError>) -> Option<String> {
    match result {
        Err(e) => Some(e.to_string()),
        Ok(_) if !vm.errors.is_empty() => Some(vm.errors.join("; ")),
        Ok(Value::Bool(false)) => Some("returned False".to_owned()),
        Ok(_) => None,
    }
}

/// A function marked `; #[test]` or `; #[mock(Script.Function)]`.
pub(crate) struct Marked {
    /// The file of the script.
    path: PathBuf,
    script: String,
    function: String,
    /// The native a mock replaces; `None` for a test.
    mocks: Option<String>,
}

/// The `#[test]` and `#[mock]` functions of `scripts`.
pub(crate) fn marked_functions(scripts: &[PathBuf]) -> Vec<Marked> {
    use papyrus_parser::ast::{self, AstNode};
    let mut out = Vec::new();
    for path in scripts {
        let Some(text) = papyrus_ide::read_file(path) else { continue };
        if !text.contains("#[") {
            continue;
        }
        let parse = papyrus_parser::parse(&text);
        let file = parse.source_file();
        let Some(script) = file.header().and_then(|h| h.name()).map(|n| n.text().to_owned()) else {
            continue;
        };
        for attr in papyrus_parser::attrs::attributes(parse.syntax_node()) {
            let mocks = match attr.path.to_ascii_lowercase().as_str() {
                "test" => None,
                "mock" => Some(attr.args.first().map(|a| a.to_string()).unwrap_or_default()),
                _ => continue,
            };
            let node = parse.syntax_node().covering_node(attr.target);
            let func = std::iter::once(node).chain(node.ancestors()).find_map(ast::Function::cast);
            if let Some(name) = func.and_then(|f| f.name()) {
                out.push(Marked {
                    path: path.clone(),
                    script: script.clone(),
                    function: name.text().to_owned(),
                    mocks,
                });
            }
        }
    }
    out
}

/// A new object of `script`, its `#[mock]` functions replacing the natives,
/// initialized by its `OnInit` event.
pub(crate) fn start(
    vm: &mut Vm,
    marked: &[Marked],
    script: &str,
) -> Result<papyrus_vm::ObjId, papyrus_vm::VmError> {
    vm.clear_mocks();
    let obj = vm.new_object(script)?;
    for m in marked.iter().filter(|m| m.script.eq_ignore_ascii_case(script)) {
        if let Some(native) = &m.mocks {
            vm.mock(native, obj, &m.function);
        }
    }
    vm.send_event(obj, "OnInit", &[])?;
    Ok(obj)
}

pub(crate) fn collect(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    collect_ext(dir, "psc", out);
}

/// The `.EXT` files below `dir`, but in hidden, `target` and `node_modules`
/// directories.
fn collect_ext(dir: &std::path::Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            if !(name.starts_with('.') || name == "target" || name == "node_modules") {
                collect_ext(&path, ext, out);
            }
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
            out.push(path);
        }
    }
}
