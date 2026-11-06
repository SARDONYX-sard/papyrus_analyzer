//! Semantic highlighting.

use papyrus_parser::{
    SyntaxKind, TextRange,
    ast::{self, AstNode},
};

use super::{decl_def, with_sema};
use crate::{
    FileId, Workspace,
    index::{Flags, MemberKind},
    sema::{Def, LocalKind, Sema},
};

/// Semantic token types. The discriminant is the index in the legend sent to
/// the client, see [`TokenType::ALL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TokenType {
    /// Keywords.
    Keyword,
    /// Comments.
    Comment,
    /// String literals.
    String,
    /// Number literals.
    Number,
    /// Operators.
    Operator,
    /// Script names.
    Class,
    /// Primitive types.
    Type,
    /// Functions.
    Function,
    /// Events.
    Event,
    /// Properties.
    Property,
    /// Variables.
    Variable,
    /// Parameters.
    Parameter,
    /// States.
    Namespace,
    /// Flags such as `Hidden`.
    Decorator,
}

impl TokenType {
    /// All token types in legend order, with their LSP names.
    pub const ALL: [(TokenType, &'static str); 14] = [
        (TokenType::Keyword, "keyword"),
        (TokenType::Comment, "comment"),
        (TokenType::String, "string"),
        (TokenType::Number, "number"),
        (TokenType::Operator, "operator"),
        (TokenType::Class, "class"),
        (TokenType::Type, "type"),
        (TokenType::Function, "function"),
        (TokenType::Event, "event"),
        (TokenType::Property, "property"),
        (TokenType::Variable, "variable"),
        (TokenType::Parameter, "parameter"),
        (TokenType::Namespace, "namespace"),
        (TokenType::Decorator, "decorator"),
    ];
}

/// Semantic token modifiers, as bits. See [`TokenModifier::ALL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TokenModifier {
    /// The declaring occurrence.
    Declaration = 1,
    /// `AutoReadOnly` properties.
    Readonly = 1 << 1,
    /// `Global` functions.
    Static = 1 << 2,
    /// Built-ins and native declarations.
    DefaultLibrary = 1 << 3,
    /// `{ ... }` documentation comments.
    Documentation = 1 << 4,
}

impl TokenModifier {
    /// All modifiers in legend order, with their LSP names.
    pub const ALL: [(TokenModifier, &'static str); 5] = [
        (TokenModifier::Declaration, "declaration"),
        (TokenModifier::Readonly, "readonly"),
        (TokenModifier::Static, "static"),
        (TokenModifier::DefaultLibrary, "defaultLibrary"),
        (TokenModifier::Documentation, "documentation"),
    ];
}

/// A highlighted range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticToken {
    /// The range, never spanning multiple lines except for comments.
    pub range: TextRange,
    /// The type.
    pub ty: TokenType,
    /// Bit set of [`TokenModifier`]s.
    pub modifiers: u32,
}

/// Semantic tokens of a file, in source order.
pub fn semantic_tokens(
    ws: &Workspace,
    file: FileId,
    range: Option<TextRange>,
) -> Vec<SemanticToken> {
    with_sema(ws, file, |sema| {
        let mut out = Vec::new();
        let fragments = papyrus_parser::attrs::fragments(sema.root);
        for tok in sema.root.descendant_tokens() {
            if range.is_some_and(|r| r.intersect(tok.text_range()).is_none()) {
                continue;
            }
            let kind = tok.kind();
            let (ty, modifiers) = match kind {
                // Attributes (`; #[allow(...)]`) keep the colors of the grammar,
                // which themes give Rust attributes (`meta.attribute.rust`).
                SyntaxKind::LINE_COMMENT if is_attribute(tok.text()) => continue,
                // So do the markers of the Creation Kit, in its fragments only:
                // elsewhere, `; END CODE` is a comment like any other.
                SyntaxKind::LINE_COMMENT
                    if papyrus_parser::attrs::fragment_marker(tok.text()).is_some()
                        && fragments.iter().any(|f| f.contains(tok.text_range().start())) =>
                {
                    continue;
                }
                SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT => (TokenType::Comment, 0),
                SyntaxKind::DOC_COMMENT => {
                    (TokenType::Comment, TokenModifier::Documentation as u32)
                }
                SyntaxKind::STRING => (TokenType::String, 0),
                SyntaxKind::INT_NUMBER | SyntaxKind::FLOAT_NUMBER => (TokenType::Number, 0),
                k if k.is_primitive_type() => (TokenType::Type, 0),
                k if k.is_keyword() => (TokenType::Keyword, 0),
                SyntaxKind::IDENT => match classify_ident(sema, tok) {
                    Some(c) => c,
                    None => continue,
                },
                SyntaxKind::EQ
                | SyntaxKind::EQ2
                | SyntaxKind::NEQ
                | SyntaxKind::LT
                | SyntaxKind::LTEQ
                | SyntaxKind::GT
                | SyntaxKind::GTEQ
                | SyntaxKind::PLUS
                | SyntaxKind::PLUSEQ
                | SyntaxKind::MINUS
                | SyntaxKind::MINUSEQ
                | SyntaxKind::STAR
                | SyntaxKind::STAREQ
                | SyntaxKind::SLASH
                | SyntaxKind::SLASHEQ
                | SyntaxKind::PERCENT
                | SyntaxKind::PERCENTEQ
                | SyntaxKind::BANG
                | SyntaxKind::AMP2
                | SyntaxKind::PIPE2 => (TokenType::Operator, 0),
                _ => continue,
            };
            out.push(SemanticToken { range: tok.text_range(), ty, modifiers });
        }
        out
    })
    .unwrap_or_default()
}

/// `; #[...]` or `; #![...]`, see [`papyrus_parser::attrs`].
fn is_attribute(comment: &str) -> bool {
    let rest = comment.strip_prefix(';').unwrap_or_default().trim_start();
    rest.starts_with("#[") || rest.starts_with("#![")
}

fn classify_ident(
    sema: &Sema<'_>,
    tok: papyrus_parser::SyntaxToken<'_>,
) -> Option<(TokenType, u32)> {
    let parent = tok.parent()?;
    let decl = TokenModifier::Declaration as u32;
    match parent.kind() {
        SyntaxKind::FLAG => Some((TokenType::Decorator, 0)),
        SyntaxKind::TYPE => Some((TokenType::Class, 0)),
        SyntaxKind::NAME => {
            let def = decl_def(sema, parent);
            let (ty, m) = def.map_or((TokenType::Variable, 0), |d| def_class(sema, d));
            Some((ty, m | decl))
        }
        SyntaxKind::NAME_REF => {
            let nr = ast::NameRef::cast(parent)?;
            match sema.resolve_name_ref(nr) {
                Some(def) => Some(def_class(sema, def)),
                None => {
                    let is_call = parent.parent().is_some_and(|p| {
                        p.kind() == SyntaxKind::CALL_EXPR
                            || (p.kind() == SyntaxKind::FIELD_EXPR
                                && p.parent().is_some_and(|c| c.kind() == SyntaxKind::CALL_EXPR))
                    });
                    Some((if is_call { TokenType::Function } else { TokenType::Variable }, 0))
                }
            }
        }
        _ => None,
    }
}

fn def_class(sema: &Sema<'_>, def: Def) -> (TokenType, u32) {
    match def {
        Def::Local { kind: LocalKind::Parameter, .. } | Def::Param { .. } => {
            (TokenType::Parameter, 0)
        }
        Def::Local { .. } => (TokenType::Variable, 0),
        Def::Script { .. } => (TokenType::Class, 0),
        Def::Flag(_) => (TokenType::Decorator, 0),
        Def::LanguageFlag(_) => (TokenType::Keyword, 0),
        Def::Builtin(b, _) => {
            let ty = if b.is_callable() { TokenType::Function } else { TokenType::Property };
            (ty, TokenModifier::DefaultLibrary as u32)
        }
        Def::Member { file, index } => {
            let m = sema.member(file, index);
            let mut mods = 0;
            if m.flags.contains(Flags::GLOBAL) {
                mods |= TokenModifier::Static as u32;
            }
            if m.flags.contains(Flags::NATIVE) {
                mods |= TokenModifier::DefaultLibrary as u32;
            }
            if m.flags.contains(Flags::AUTO_READONLY) {
                mods |= TokenModifier::Readonly as u32;
            }
            let ty = match m.kind {
                MemberKind::Function => TokenType::Function,
                MemberKind::Event => TokenType::Event,
                MemberKind::Property => TokenType::Property,
                MemberKind::Variable => TokenType::Variable,
                MemberKind::State => TokenType::Namespace,
            };
            (ty, mods)
        }
    }
}
