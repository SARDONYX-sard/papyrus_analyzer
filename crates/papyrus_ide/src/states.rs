//! The states named by strings: `GoToState("Busy")`, `GetState() == "Busy"`.
//!
//! What a string names depends on the object:
//!
//! - `GoToState("x")`, `Self.GoToState("x")`: a state of the script, of a parent,
//!   or of a script extending it, which `Self` may be;
//! - `akOther.GoToState("x")`: a state of the type of `akOther` or of a parent.
//!   The object may be of any script extending the type, which is only known
//!   at run time: `ObjectReference` has thousands, unrelated to this code.
//!
//! Other string expressions are not followed.
//!
//! These strings are references like identifiers are: hover, go to definition,
//! references and rename work on them, through [`state_def_at`].

use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind, SyntaxToken, TextRange, TextSize,
    ast::{self, AstNode},
};

use crate::{
    FileId,
    index::{MemberKind, ScriptIndex},
    sema::{Builtin, Def, Sema, Ty},
};

/// How a string names a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateUse {
    /// `GoToState("Busy")`
    GoTo,
    /// `GetState() == "Busy"`, or `!=`.
    Compare,
}

/// A string literal naming a state.
#[derive(Clone, Copy, Debug)]
pub struct StateRef<'a> {
    /// The string token, with its quotes.
    pub token: SyntaxToken<'a>,
    /// The text between the quotes.
    pub name: &'a str,
    /// The range of `name`.
    pub name_range: TextRange,
    /// The script of the object whose state it is.
    pub script: Name,
    /// The object is `Self`: the scripts extending `script` count too.
    pub on_self: bool,
    /// How it is used.
    pub usage: StateUse,
}

/// The state named by the string `token`: an argument of `GoToState`, or
/// compared with `GetState()`.
pub fn state_ref<'a>(sema: &Sema<'a>, token: SyntaxToken<'a>) -> Option<StateRef<'a>> {
    if token.kind() != SyntaxKind::STRING {
        return None;
    }
    let literal = token.parent()?;
    let parent = literal.parent()?;
    let (usage, call) = if let Some(arg) = ast::Arg::cast(parent) {
        if arg.name().is_some_and(|n| !n.text().eq_ignore_ascii_case("asNewState")) {
            return None;
        }
        (StateUse::GoTo, arg.syntax().parent()?.parent().and_then(ast::CallExpr::cast)?)
    } else {
        let bin = ast::BinExpr::cast(parent)?;
        if !matches!(bin.op()?.kind(), SyntaxKind::EQ2 | SyntaxKind::NEQ) {
            return None;
        }
        let other = if bin.lhs()?.syntax() == literal { bin.rhs()? } else { bin.lhs()? };
        let ast::Expr::Call(call) = other else { return None };
        (StateUse::Compare, call)
    };
    let builtin = match usage {
        StateUse::GoTo => Builtin::GoToState,
        StateUse::Compare => Builtin::GetState,
    };
    if !matches!(sema.resolve_name_ref(call.callee_name()?)?, Def::Builtin(b, _) if b == builtin) {
        return None;
    }
    let (script, on_self) = match call.callee()? {
        ast::Expr::Field(field) => {
            let receiver = field.receiver()?;
            let on_self = matches!(receiver, ast::Expr::NameRef(n) if n.is_self());
            match sema.infer(receiver)? {
                Ty::Value(ty) => (ty.object()?, on_self),
                _ => return None,
            }
        }
        _ => (sema.script_name()?, true),
    };
    let text = token.text();
    let inner = text.strip_prefix('"')?;
    // Without closing quote while it is being typed.
    let name = inner.strip_suffix('"').unwrap_or(inner);
    if name.contains('\\') {
        return None;
    }
    let start = token.text_range().start() + TextSize::new(1);
    let name_range = TextRange::at(start, TextSize::of(name));
    Some(StateRef { token, name, name_range, script, on_self, usage })
}

/// The state named by the string around `offset`, between its quotes.
pub fn state_ref_at<'a>(sema: &Sema<'a>, offset: TextSize) -> Option<StateRef<'a>> {
    let token = sema.root.token_at_offset(offset).max_by_key(|t| t.kind() == SyntaxKind::STRING)?;
    state_ref(sema, token).filter(|r| r.name_range.contains_inclusive(offset))
}

/// A state declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateDecl {
    /// The name, as declared.
    pub name: Name,
    /// The file declaring it.
    pub file: FileId,
    /// Its index in the members of the file.
    pub index: u32,
    /// In a script extending the script, not in the script or a parent.
    pub in_child: bool,
}

fn states_of(file: FileId, index: &ScriptIndex, in_child: bool) -> impl Iterator<Item = StateDecl> {
    let states = index.members.iter().enumerate().filter(|(_, m)| m.kind == MemberKind::State);
    states.map(move |(i, m)| StateDecl { name: m.name, file, index: i as u32, in_child })
}

/// The states an object of `script` may go to: of the script and its parents,
/// the nearest first, then of the scripts extending it.
pub fn declarations(sema: &Sema<'_>, script: Name) -> Vec<StateDecl> {
    let mut out: Vec<StateDecl> =
        sema.chain(script).flat_map(|(file, index)| states_of(file, index, false)).collect();
    for (name, file) in sema.ws.all_scripts() {
        let index = &sema.ws.file(file).index;
        if name != script
            && index.members.iter().any(|m| m.kind == MemberKind::State)
            && sema.chain(name).any(|(_, i)| i.name == Some(script))
        {
            out.extend(states_of(file, index, true));
        }
    }
    out
}

/// The states the string `state` may name, see the module documentation.
pub fn candidates(sema: &Sema<'_>, state: &StateRef<'_>) -> Vec<StateDecl> {
    if state.on_self {
        return declarations(sema, state.script);
    }
    sema.chain(state.script).flat_map(|(file, index)| states_of(file, index, false)).collect()
}

/// The declarations of the state named by the string `state`, like
/// [`candidates`] of that name. Go to definition and hover ask this: of all the
/// known scripts, only those mentioning the name are looked at.
pub(crate) fn candidates_named(sema: &Sema<'_>, state: &StateRef<'_>) -> Vec<StateDecl> {
    let name = Name::new(state.name);
    let named = |file: FileId, index: &ScriptIndex, in_child: bool| {
        states_of(file, index, in_child).filter(|d| d.name == name).collect::<Vec<_>>()
    };
    let mut out: Vec<StateDecl> =
        sema.chain(state.script).flat_map(|(file, index)| named(file, index, false)).collect();
    if state.on_self {
        for (file, data) in sema.ws.files() {
            let index = &data.index;
            if index.mentions(name)
                && index.find(name, |m| m.kind == MemberKind::State).is_some()
                && index.name.is_some_and(|other| {
                    other != state.script
                        && sema.ws.script(other) == Some(file)
                        && sema.chain(other).any(|(_, i)| i.name == Some(state.script))
                })
            {
                out.extend(named(file, index, true));
            }
        }
    }
    out
}

/// The declaration of the state named by the string at `offset`, the nearest
/// one up the `extends` chain, with the range of the name.
pub(crate) fn state_def_at(sema: &Sema<'_>, offset: TextSize) -> Option<(TextRange, Def)> {
    let state = state_ref_at(sema, offset)?;
    // Up the chain first: most strings name a state of the script or a parent.
    let name = Name::new(state.name);
    let own = sema.chain(state.script).find_map(|(file, index)| {
        let i = index.find(name, |m| m.kind == MemberKind::State)?;
        Some(Def::Member { file, index: i as u32 })
    });
    let def = own.or_else(|| {
        let decl = candidates_named(sema, &state).into_iter().next()?;
        Some(Def::Member { file: decl.file, index: decl.index })
    })?;
    Some((state.name_range, def))
}

/// Whether the string `state`, of the same name, names the state declared by
/// the script of `file`. Follows the `extends` chains only: searches ask this of
/// every string spelled like the state.
pub(crate) fn refers_to(sema: &Sema<'_>, state: &StateRef<'_>, file: FileId) -> bool {
    if sema.chain(state.script).any(|(f, _)| f == file) {
        return true;
    }
    // Below the script: `Self` may be it; another object only overrides a state
    // its type has.
    let is_below =
        sema.ws.file(file).index.name.is_some_and(|declaring| {
            sema.chain(declaring).any(|(_, i)| i.name == Some(state.script))
        });
    is_below
        && (state.on_self || {
            let name = Name::new(state.name);
            sema.chain(state.script)
                .any(|(_, i)| i.find(name, |m| m.kind == MemberKind::State).is_some())
        })
}

/// The declarations sharing the state declared at `file`, `index`, for rename:
/// those of the topmost script of the chain declaring it, and below it.
pub(crate) fn state_family(sema: &Sema<'_>, file: FileId, index: u32) -> Vec<Def> {
    let name = sema.member(file, index).name;
    let is_state = |i: &ScriptIndex| i.find(name, |m| m.kind == MemberKind::State).is_some();
    let script = sema.ws.file(file).index.name;
    let top = script.and_then(|s| sema.chain(s).filter(|(_, i)| is_state(i)).last());
    let mut out: Vec<Def> = top
        .and_then(|(_, i)| i.name)
        .map(|top| declarations(sema, top))
        .unwrap_or_default()
        .into_iter()
        .filter(|d| d.name == name)
        .map(|d| Def::Member { file: d.file, index: d.index })
        .collect();
    let def = Def::Member { file, index };
    if !out.contains(&def) {
        out.push(def);
    }
    out
}

/// Whether a state of an object of `script` is called `state`. Lints ask this
/// of every `GoToState`: the scripts extending it are searched last.
pub fn is_declared(sema: &Sema<'_>, script: Name, state: &str) -> bool {
    let is_state = |index: &ScriptIndex| {
        index
            .members
            .iter()
            .any(|m| m.kind == MemberKind::State && m.name.as_str().eq_ignore_ascii_case(state))
    };
    sema.chain(script).any(|(_, index)| is_state(index))
        || sema.ws.all_scripts().any(|(name, file)| {
            is_state(&sema.ws.file(file).index)
                && sema.chain(name).any(|(_, i)| i.name == Some(script))
        })
}
