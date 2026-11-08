//! Name resolution and type inference.
//!
//! Papyrus scoping rules, from innermost to outermost:
//!
//! 1. local variables declared earlier in the enclosing blocks;
//! 2. parameters of the enclosing function or event;
//! 3. variables and properties of the script (variables are private);
//! 4. properties of the parent scripts, following `extends`;
//! 5. script names, used as types or as receivers of `Global` functions.
//!
//! Functions are looked up in the script, its parents, the built-in state
//! functions and the `Global` functions of imported scripts.

use std::sync::Arc;

use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind, SyntaxNode, SyntaxTree, TextRange, TextSize,
    ast::{self, AstNode},
};

use crate::{
    FileId, Workspace,
    index::{BaseType, Member, MemberKind, ScriptIndex, TypeRef},
};

/// A built-in function or property that is not declared in any script.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Builtin {
    /// `array.Length`
    Length,
    /// `array.Find(value, start = 0)`
    Find,
    /// `array.RFind(value, start = -1)`
    RFind,
    /// `GoToState(name)`
    GoToState,
    /// `GetState()`
    GetState,
    /// `Event OnBeginState()`
    OnBeginState,
    /// `Event OnEndState()`
    OnEndState,
}

impl Builtin {
    const ARRAY: [Builtin; 3] = [Builtin::Length, Builtin::Find, Builtin::RFind];
    const SCRIPT: [Builtin; 2] = [Builtin::GoToState, Builtin::GetState];

    /// The name of the built-in.
    pub fn name(self) -> &'static str {
        match self {
            Builtin::Length => "Length",
            Builtin::Find => "Find",
            Builtin::RFind => "RFind",
            Builtin::GoToState => "GoToState",
            Builtin::GetState => "GetState",
            Builtin::OnBeginState => "OnBeginState",
            Builtin::OnEndState => "OnEndState",
        }
    }

    /// Pseudo source of the declaration, for hovers.
    pub fn signature(self, element: Option<TypeRef>) -> String {
        let elem = element.map_or_else(|| "Var".to_owned(), |t| t.to_string());
        match self {
            Builtin::Length => "Int Length".to_owned(),
            Builtin::Find => format!("Int Function Find({elem} akElement, Int aiStartIndex = 0)"),
            Builtin::RFind => {
                format!("Int Function RFind({elem} akElement, Int aiStartIndex = -1)")
            }
            Builtin::GoToState => "Function GoToState(String asNewState)".to_owned(),
            Builtin::GetState => "String Function GetState()".to_owned(),
            Builtin::OnBeginState => "Event OnBeginState()".to_owned(),
            Builtin::OnEndState => "Event OnEndState()".to_owned(),
        }
    }

    /// The documentation, with `# Examples`, as readers see it.
    pub fn docs(self) -> String {
        crate::index::hide_test_lines(self.docs_source())
    }

    /// The documentation as written, Markdown files of `builtin_docs/`: the
    /// examples with their lines for doc tests.
    pub fn docs_source(self) -> &'static str {
        match self {
            Builtin::Length => include_str!("builtin_docs/length.md"),
            Builtin::Find => include_str!("builtin_docs/find.md"),
            Builtin::RFind => include_str!("builtin_docs/rfind.md"),
            Builtin::GoToState => include_str!("builtin_docs/gotostate.md"),
            Builtin::GetState => include_str!("builtin_docs/getstate.md"),
            Builtin::OnBeginState => include_str!("builtin_docs/onbeginstate.md"),
            Builtin::OnEndState => include_str!("builtin_docs/onendstate.md"),
        }
    }

    /// The type of the value or of the call result.
    pub fn ty(self) -> Option<TypeRef> {
        match self {
            Builtin::Length | Builtin::Find | Builtin::RFind => Some(TypeRef::INT),
            Builtin::GetState => Some(TypeRef::STRING),
            Builtin::GoToState | Builtin::OnBeginState | Builtin::OnEndState => None,
        }
    }

    /// Parameter names and labels, for signature help.
    pub fn params(self, element: Option<TypeRef>) -> Vec<String> {
        let elem = element.map_or_else(|| "Var".to_owned(), |t| t.to_string());
        match self {
            Builtin::Find => vec![format!("{elem} akElement"), "Int aiStartIndex = 0".to_owned()],
            Builtin::RFind => vec![format!("{elem} akElement"), "Int aiStartIndex = -1".to_owned()],
            Builtin::GoToState => vec!["String asNewState".to_owned()],
            _ => Vec::new(),
        }
    }

    /// Returns `true` for functions.
    pub fn is_callable(self) -> bool {
        self != Builtin::Length
    }

    fn lookup(list: &[Builtin], name: &str) -> Option<Builtin> {
        list.iter().copied().find(|b| b.name().eq_ignore_ascii_case(name))
    }
}

/// Whether a local is a variable or a parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocalKind {
    /// A local variable.
    Variable,
    /// A function or event parameter.
    Parameter,
}

/// What a name refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Def {
    /// A local variable or parameter of the current function.
    Local {
        /// The file declaring it.
        file: FileId,
        /// The name.
        name: Name,
        /// The range of the declaring name.
        range: TextRange,
        /// The declared type.
        ty: Option<TypeRef>,
        /// Variable or parameter.
        kind: LocalKind,
    },
    /// A script member.
    Member {
        /// The file declaring it.
        file: FileId,
        /// Index into [`ScriptIndex::members`].
        index: u32,
    },
    /// A parameter of a script function, referenced by a named argument.
    Param {
        /// The file declaring it.
        file: FileId,
        /// Index into [`ScriptIndex::members`].
        member: u32,
        /// Index into [`Member::params`].
        param: u32,
    },
    /// A script.
    Script {
        /// The file declaring it.
        file: FileId,
    },
    /// A built-in.
    Builtin(Builtin, Option<TypeRef>),
    /// A user flag, by index into [`crate::flags::FlagTable::defs`].
    Flag(u16, u32),
    /// A language flag: `Native`, `Global`, `Auto` or `AutoReadOnly`.
    LanguageFlag(SyntaxKind),
}

/// The type of an expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    /// A value of the given type.
    Value(TypeRef),
    /// A script name used as a receiver, e.g. `Debug` in `Debug.Trace()`.
    Static(Name),
    /// The `None` literal.
    None,
}

/// Which members a lookup accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// Functions and events.
    Callable,
    /// Properties, and variables when `private` is set.
    Value {
        /// Include script variables, which are private to their script.
        private: bool,
    },
}

impl Want {
    fn accepts(self, m: &Member) -> bool {
        match self {
            Want::Callable => m.is_callable(),
            Want::Value { private } => {
                m.kind == MemberKind::Property || (private && m.kind == MemberKind::Variable)
            }
        }
    }
}

/// Owned text and tree of a file, for files that are not open.
pub struct FileSource {
    text: Arc<str>,
    tree: Arc<SyntaxTree>,
}

impl FileSource {
    /// Gets the text and tree of `file`, parsing it if it is not open.
    pub fn load(ws: &Workspace, file: FileId) -> Option<FileSource> {
        let data = ws.file(file);
        if let Some(doc) = &data.open {
            return Some(FileSource { text: doc.text.clone(), tree: doc.tree.clone() });
        }
        if let Some(doc) = &data.disk {
            return Some(FileSource { text: doc.text.clone(), tree: doc.tree.clone() });
        }
        Some(FileSource::from_text(ws.text(file)?))
    }

    /// Like [`FileSource::load`], with the syntax errors: for checking the
    /// file and then working on it without parsing it again.
    pub fn load_with_errors(
        ws: &Workspace,
        file: FileId,
    ) -> Option<(FileSource, Vec<papyrus_parser::SyntaxError>)> {
        let data = ws.file(file);
        if let Some(doc) = &data.open {
            let src = FileSource { text: doc.text.clone(), tree: doc.tree.clone() };
            return Some((src, doc.errors.to_vec()));
        }
        if let Some(doc) = &data.disk {
            let src = FileSource { text: doc.text.clone(), tree: doc.tree.clone() };
            return Some((src, doc.errors.to_vec()));
        }
        Some(FileSource::parse(ws.text(file)?))
    }

    /// Parses `text`.
    pub fn from_text(text: Arc<str>) -> FileSource {
        FileSource::parse(text).0
    }

    /// Parses `text`, keeping the syntax errors.
    pub fn parse(text: Arc<str>) -> (FileSource, Vec<papyrus_parser::SyntaxError>) {
        let (tree, errors) = papyrus_parser::parse(&text).into_parts();
        (FileSource { text, tree: Arc::new(tree) }, errors.into_iter().collect())
    }

    /// The text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Binds the source to the workspace for semantic queries.
    pub fn sema<'a>(&'a self, ws: &'a Workspace, file: FileId) -> Sema<'a> {
        Sema {
            ws,
            file,
            text: &self.text,
            root: self.tree.root(&self.text),
            index: &ws.file(file).index,
        }
    }
}

/// Semantic queries inside one file.
#[derive(Clone, Copy)]
pub struct Sema<'a> {
    /// The workspace.
    pub ws: &'a Workspace,
    /// The file.
    pub file: FileId,
    /// The text of the file.
    pub text: &'a str,
    /// The root of the syntax tree.
    pub root: SyntaxNode<'a>,
    /// The index of the file.
    pub index: &'a ScriptIndex,
}

/// Upper bound on `extends` chains, protecting against cycles.
const MAX_CHAIN: usize = 64;

impl<'a> Sema<'a> {
    /// The member at `index` of `file`.
    pub fn member(&self, file: FileId, index: u32) -> &'a Member {
        &self.ws.file(file).index.members[index as usize]
    }

    /// The name of the current script.
    pub fn script_name(&self) -> Option<Name> {
        self.index.name
    }

    /// Walks the `extends` chain of `script`, the script itself included.
    pub fn chain(&self, script: Name) -> impl Iterator<Item = (FileId, &'a ScriptIndex)> + 'a {
        let ws = self.ws;
        let mut next = Some(script);
        let mut near = self.file;
        let mut seen = Vec::new();
        std::iter::from_fn(move || {
            let name = next.take()?;
            if seen.contains(&name) || seen.len() >= MAX_CHAIN {
                return None;
            }
            seen.push(name);
            let file = ws.script_near(name, near)?;
            near = file;
            let index: &'a ScriptIndex = &ws.file(file).index;
            next = index.extends;
            Some((file, index))
        })
    }

    /// Returns `true` if every script of the chain of `script` is known, which
    /// makes "unresolved member" diagnostics trustworthy.
    pub fn chain_is_complete(&self, script: Name) -> bool {
        let mut last = None;
        let mut len = 0;
        for (_, index) in self.chain(script) {
            last = Some(index);
            len += 1;
        }
        match last {
            Some(index) => len < MAX_CHAIN && index.extends.is_none(),
            None => false,
        }
    }

    /// Finds member `name` of `script` or of its parents.
    pub fn lookup_member(&self, script: Name, name: Name, want: Want) -> Option<(FileId, u32)> {
        for (depth, (file, index)) in self.chain(script).enumerate() {
            // Variables are private: only visible in the declaring script.
            let want = match want {
                Want::Value { private } => Want::Value { private: private && depth == 0 },
                other => other,
            };
            if let Some(i) = index.find(name, |m| want.accepts(m)) {
                return Some((file, i as u32));
            }
        }
        None
    }

    /// Resolves a reference.
    pub fn resolve_name_ref(&self, nr: ast::NameRef<'a>) -> Option<Def> {
        let node = nr.syntax();
        let name = Name::new(nr.text());

        if nr.is_self() {
            return self.script_def(self.script_name()?);
        }
        if nr.is_parent() {
            return self.script_def(self.index.extends?);
        }

        let parent = node.parent()?;
        match parent.kind() {
            SyntaxKind::EXTENDS_CLAUSE | SyntaxKind::IMPORT => self.script_def(name),
            SyntaxKind::FIELD_EXPR => {
                let field = ast::FieldExpr::cast(parent)?;
                if field.name_ref() == Some(nr) {
                    let want = if is_callee(parent) {
                        Want::Callable
                    } else {
                        Want::Value { private: false }
                    };
                    self.resolve_member_of(self.infer(field.receiver()?)?, name, want)
                } else {
                    self.resolve_value(name, node)
                }
            }
            SyntaxKind::ARG if ast::Arg::cast(parent)?.name() == Some(nr) => {
                let call = parent.parent()?.parent().and_then(ast::CallExpr::cast)?;
                let Def::Member { file, index } = self.resolve_name_ref(call.callee_name()?)?
                else {
                    return None;
                };
                let member = self.member(file, index);
                let param = member.params.iter().position(|p| p.name == name)?;
                Some(Def::Param { file, member: index, param: param as u32 })
            }
            _ if is_callee(node) => self.resolve_function(name),
            _ => self.resolve_value(name, node),
        }
    }

    /// Resolves the base of a type to a script.
    pub fn resolve_type(&self, ty: ast::Type<'a>) -> Option<Def> {
        if ty.is_primitive() {
            return None;
        }
        self.script_def(Name::new(ty.base_text()))
    }

    fn script_def(&self, name: Name) -> Option<Def> {
        Some(Def::Script { file: self.ws.script_near(name, self.file)? })
    }

    /// Resolves a function called without receiver.
    pub fn resolve_function(&self, name: Name) -> Option<Def> {
        if let Some(script) = self.script_name() {
            if let Some((file, index)) = self.lookup_member(script, name, Want::Callable) {
                return Some(Def::Member { file, index });
            }
        } else if let Some(i) = self.index.find(name, |m| m.is_callable()) {
            return Some(Def::Member { file: self.file, index: i as u32 });
        }
        if let Some(b) = Builtin::lookup(&Builtin::SCRIPT, name.as_str()) {
            return Some(Def::Builtin(b, None));
        }
        for &import in self.index.imports.iter() {
            let Some(file) = self.ws.script_near(import, self.file) else { continue };
            let index = &self.ws.file(file).index;
            if let Some(i) = index.find(name, |m| m.is_callable() && m.is_global()) {
                return Some(Def::Member { file, index: i as u32 });
            }
        }
        None
    }

    /// Resolves a value name at `node`.
    pub fn resolve_value(&self, name: Name, node: SyntaxNode<'a>) -> Option<Def> {
        if let Some(local) = self.find_local(name, node) {
            return Some(local);
        }
        let own = match self.script_name() {
            Some(script) => self.lookup_member(script, name, Want::Value { private: true }),
            None => self
                .index
                .find(name, |m| matches!(m.kind, MemberKind::Property | MemberKind::Variable))
                .map(|i| (self.file, i as u32)),
        };
        if let Some((file, index)) = own {
            return Some(Def::Member { file, index });
        }
        self.script_def(name)
    }

    fn resolve_member_of(&self, recv: Ty, name: Name, want: Want) -> Option<Def> {
        match recv {
            Ty::Value(ty) if ty.array => {
                let b = Builtin::lookup(&Builtin::ARRAY, name.as_str())?;
                (b.is_callable() == (want == Want::Callable))
                    .then_some(Def::Builtin(b, ty.element()))
            }
            Ty::Value(TypeRef { base: BaseType::Object(script), .. }) => {
                let private = Some(script) == self.script_name();
                let want = match want {
                    Want::Value { .. } => Want::Value { private },
                    w => w,
                };
                if let Some((file, index)) = self.lookup_member(script, name, want) {
                    return Some(Def::Member { file, index });
                }
                if want == Want::Callable
                    && let Some(b) = Builtin::lookup(&Builtin::SCRIPT, name.as_str())
                {
                    return Some(Def::Builtin(b, None));
                }
                None
            }
            Ty::Static(script) => {
                let (file, index) = self.lookup_member(script, name, Want::Callable)?;
                Some(Def::Member { file, index })
            }
            Ty::Value(_) | Ty::None => None,
        }
    }

    /// The local variable or parameter `name` seen at `node`.
    ///
    /// The compiler hoists locals to their block: a variable declared later
    /// in an enclosing block is visible too, but earlier ones win. So the
    /// scopes are searched from the innermost (blocks, then the parameters)
    /// for the last declaration before `node`; failing that, for the last
    /// declaration of the innermost scope declaring `name` at all. This is
    /// [`Sema::locals_at`] searched in reverse, twice, without building it.
    pub fn find_local(&self, name: Name, node: SyntaxNode<'a>) -> Option<Def> {
        let offset = node.text_range().start();
        let is_name = |n: &str| n.eq_ignore_ascii_case(name.as_str());
        // A declaration: its name, type and kind.
        type Decl<'n> = (ast::Name<'n>, Option<ast::Type<'n>>, LocalKind);
        let mut before: Option<Decl<'a>> = None;
        let mut anywhere: Option<Decl<'a>> = None;
        for anc in std::iter::once(node).chain(node.ancestors()) {
            // The last declaration of `name` in this scope, before `node` and
            // anywhere.
            let mut scope_before = None;
            let mut scope_anywhere = None;
            // Parameters are visible everywhere in the function.
            let mut see = |decl: Decl<'a>| {
                if decl.2 == LocalKind::Parameter || decl.0.syntax().text_range().end() <= offset {
                    scope_before = Some(decl);
                }
                scope_anywhere = Some(decl);
            };
            let last_scope = match anc.kind() {
                SyntaxKind::BLOCK => {
                    for var in anc.children().filter_map(ast::VarDecl::cast) {
                        if let Some(n) = var.name().filter(|n| is_name(n.text())) {
                            see((n, var.ty(), LocalKind::Variable));
                        }
                    }
                    false
                }
                SyntaxKind::FUNCTION | SyntaxKind::EVENT => {
                    let list =
                        anc.child_node(SyntaxKind::PARAM_LIST).and_then(ast::ParamList::cast);
                    for p in list.into_iter().flat_map(|l| l.params()) {
                        if let Some(n) = p.name().filter(|n| is_name(n.text())) {
                            see((n, p.ty(), LocalKind::Parameter));
                        }
                    }
                    true
                }
                _ => continue,
            };
            if scope_before.is_some() {
                before = scope_before;
                break;
            }
            anywhere = anywhere.or(scope_anywhere);
            if last_scope {
                break;
            }
        }
        let (n, ty, kind) = before.or(anywhere)?;
        Some(Def::Local {
            file: self.file,
            name: Name::new(n.text()),
            range: n.syntax().text_range(),
            ty: ty.and_then(TypeRef::from_ast),
            kind,
        })
    }

    /// Local variables and parameters visible at `offset`, outermost first.
    pub fn locals_at(
        &self,
        node: SyntaxNode<'a>,
        offset: TextSize,
    ) -> Vec<(Name, TextRange, Option<TypeRef>, LocalKind)> {
        let mut scopes = Vec::new();
        for anc in std::iter::once(node).chain(node.ancestors()) {
            match anc.kind() {
                SyntaxKind::BLOCK => {
                    let mut scope = Vec::new();
                    for stmt in anc.children() {
                        let Some(var) = ast::VarDecl::cast(stmt) else { continue };
                        let Some(name) = var.name() else { continue };
                        if name.syntax().text_range().end() <= offset {
                            scope.push((
                                Name::new(name.text()),
                                name.syntax().text_range(),
                                var.ty().and_then(TypeRef::from_ast),
                                LocalKind::Variable,
                            ));
                        }
                    }
                    scopes.push(scope);
                }
                SyntaxKind::FUNCTION | SyntaxKind::EVENT => {
                    let list =
                        anc.child_node(SyntaxKind::PARAM_LIST).and_then(ast::ParamList::cast);
                    let scope = list
                        .into_iter()
                        .flat_map(|l| l.params())
                        .filter_map(|p| {
                            let name = p.name()?;
                            Some((
                                Name::new(name.text()),
                                name.syntax().text_range(),
                                p.ty().and_then(TypeRef::from_ast),
                                LocalKind::Parameter,
                            ))
                        })
                        .collect();
                    scopes.push(scope);
                    break;
                }
                _ => {}
            }
        }
        scopes.into_iter().rev().flatten().collect()
    }

    /// The value type of a definition.
    pub fn def_ty(&self, def: Def) -> Option<Ty> {
        match def {
            Def::Local { ty, .. } => ty.map(Ty::Value),
            Def::Member { file, index } => self.member(file, index).ty.map(Ty::Value),
            Def::Param { file, member, param } => {
                self.member(file, member).params[param as usize].ty.map(Ty::Value)
            }
            Def::Script { file } => self.ws.file(file).index.name.map(Ty::Static),
            Def::Builtin(b, _) => b.ty().map(Ty::Value),
            Def::Flag(..) | Def::LanguageFlag(_) => None,
        }
    }

    /// Infers the type of an expression.
    pub fn infer(&self, expr: ast::Expr<'a>) -> Option<Ty> {
        use ast::Expr;
        match expr {
            Expr::Literal(lit) => {
                let ty = match lit.kind()? {
                    ast::LiteralKind::Int => TypeRef::INT,
                    ast::LiteralKind::Float => TypeRef::FLOAT,
                    ast::LiteralKind::String => TypeRef::STRING,
                    ast::LiteralKind::Bool => TypeRef::BOOL,
                    ast::LiteralKind::None => return Some(Ty::None),
                };
                Some(Ty::Value(ty))
            }
            Expr::NameRef(nr) if nr.is_self() => {
                Some(Ty::Value(TypeRef::scalar(BaseType::Object(self.script_name()?))))
            }
            Expr::NameRef(nr) if nr.is_parent() => {
                Some(Ty::Value(TypeRef::scalar(BaseType::Object(self.index.extends?))))
            }
            Expr::NameRef(nr) => self.def_ty(self.resolve_name_ref(nr)?),
            Expr::Field(field) => self.def_ty(self.resolve_name_ref(field.name_ref()?)?),
            Expr::Call(call) => match self.resolve_name_ref(call.callee_name()?)? {
                Def::Member { file, index } => self.member(file, index).ty.map(Ty::Value),
                Def::Builtin(b, _) => b.ty().map(Ty::Value),
                _ => None,
            },
            Expr::Index(index) => match self.infer(index.base()?)? {
                Ty::Value(ty) => ty.element().map(Ty::Value),
                _ => None,
            },
            Expr::Cast(cast) => TypeRef::from_ast(cast.ty()?).map(Ty::Value),
            Expr::New(new) => {
                let elem = TypeRef::from_ast(new.ty()?)?;
                Some(Ty::Value(TypeRef { base: elem.base, array: true }))
            }
            Expr::Paren(p) => self.infer(p.expr()?),
            Expr::Prefix(p) => {
                if p.op()?.kind() == SyntaxKind::BANG {
                    Some(Ty::Value(TypeRef::BOOL))
                } else {
                    self.infer(p.expr()?)
                }
            }
            Expr::Bin(bin) => {
                let op = bin.op()?.kind();
                match op {
                    SyntaxKind::PIPE2
                    | SyntaxKind::AMP2
                    | SyntaxKind::EQ2
                    | SyntaxKind::NEQ
                    | SyntaxKind::LT
                    | SyntaxKind::LTEQ
                    | SyntaxKind::GT
                    | SyntaxKind::GTEQ => Some(Ty::Value(TypeRef::BOOL)),
                    _ => {
                        let l = bin.lhs().and_then(|e| self.infer(e));
                        let r = bin.rhs().and_then(|e| self.infer(e));
                        let is = |t: TypeRef| l == Some(Ty::Value(t)) || r == Some(Ty::Value(t));
                        if op == SyntaxKind::PLUS && is(TypeRef::STRING) {
                            Some(Ty::Value(TypeRef::STRING))
                        } else if is(TypeRef::FLOAT) {
                            Some(Ty::Value(TypeRef::FLOAT))
                        } else {
                            l.or(r)
                        }
                    }
                }
            }
        }
    }

    /// The members accessible on a receiver of type `ty`, for completion.
    /// Members of child scripts shadow members of their parents.
    pub fn members_of(&self, ty: Ty) -> Vec<MemberItem<'a>> {
        let mut out: Vec<MemberItem<'a>> = Vec::new();
        let push = |item: MemberItem<'a>, out: &mut Vec<MemberItem<'a>>| {
            if !out.iter().any(|o| o.name().eq_ignore_ascii_case(item.name())) {
                out.push(item);
            }
        };
        match ty {
            Ty::Value(t) if t.array => {
                for b in Builtin::ARRAY {
                    push(MemberItem::Builtin(b, t.element()), &mut out);
                }
            }
            Ty::Value(TypeRef { base: BaseType::Object(script), .. }) => {
                let own = Some(script) == self.script_name();
                for (file, index) in self.chain(script) {
                    let private = own && index.name == Some(script);
                    for (i, m) in index.empty_state_members() {
                        let ok = match m.kind {
                            MemberKind::Function | MemberKind::Event => !m.is_global(),
                            MemberKind::Property => true,
                            MemberKind::Variable => private,
                            MemberKind::State => false,
                        };
                        if ok {
                            push(MemberItem::Member(file, i as u32, m), &mut out);
                        }
                    }
                }
                for b in Builtin::SCRIPT {
                    push(MemberItem::Builtin(b, None), &mut out);
                }
            }
            Ty::Static(script) => {
                for (file, index) in self.chain(script).take(1) {
                    for (i, m) in index.empty_state_members() {
                        if m.kind == MemberKind::Function && m.is_global() {
                            push(MemberItem::Member(file, i as u32, m), &mut out);
                        }
                    }
                }
            }
            Ty::Value(_) | Ty::None => {}
        }
        out
    }
}

/// A member offered by [`Sema::members_of`].
#[derive(Clone, Copy, Debug)]
pub enum MemberItem<'a> {
    /// A declared member.
    Member(FileId, u32, &'a Member),
    /// A built-in.
    Builtin(Builtin, Option<TypeRef>),
}

impl MemberItem<'_> {
    /// The member name.
    pub fn name(&self) -> &'static str {
        match self {
            MemberItem::Member(_, _, m) => m.name.as_str(),
            MemberItem::Builtin(b, _) => b.name(),
        }
    }
}

/// Returns `true` if `node` is the callee of a call expression.
fn is_callee(node: SyntaxNode<'_>) -> bool {
    node.parent()
        .and_then(ast::CallExpr::cast)
        .and_then(|c| c.callee())
        .is_some_and(|c| c.syntax() == node)
}
