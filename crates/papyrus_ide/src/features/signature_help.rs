//! Signature help: the parameters of the function being called.

use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind, TextSize,
    ast::{self, AstNode},
};

use super::{docs_in, with_sema};
use crate::{FileId, Workspace, index::render_param, sema::Def};

/// The result of [`signature_help`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureHelp {
    /// The whole signature.
    pub label: String,
    /// Byte ranges of each parameter inside `label`.
    pub params: Vec<(u32, u32)>,
    /// The parameter the cursor is on.
    pub active_param: Option<u32>,
    /// Markdown documentation.
    pub documentation: Option<String>,
}

/// The signature of the call surrounding `offset`.
pub fn signature_help(ws: &Workspace, file: FileId, offset: TextSize) -> Option<SignatureHelp> {
    with_sema(ws, file, |sema| {
        let tok = sema.root.token_at_offset(offset).left_biased()?;
        let arg_list = std::iter::once(tok.parent()?)
            .chain(tok.ancestors())
            .filter_map(ast::ArgList::cast)
            .find(|list| {
                let open = list.syntax().child_token(SyntaxKind::L_PAREN);
                let close = list.syntax().child_token(SyntaxKind::R_PAREN);
                open.is_some_and(|o| o.text_range().end() <= offset)
                    && close.is_none_or(|c| offset <= c.text_range().start())
            })?;
        let call = arg_list.syntax().parent().and_then(ast::CallExpr::cast)?;
        let def = sema.resolve_name_ref(call.callee_name()?)?;

        let commas = arg_list
            .syntax()
            .child_tokens()
            .filter(|t| t.kind() == SyntaxKind::COMMA && t.text_range().start() < offset)
            .count() as u32;
        // A named argument selects its parameter by name.
        let named = arg_list
            .args()
            .find(|a| a.syntax().text_range().contains_inclusive(offset))
            .and_then(|a| a.name())
            .map(|n| Name::new(n.text()));

        let (head, params, docs, names): (String, Vec<String>, Option<String>, Vec<Name>) =
            match def {
                Def::Member { file, index } => {
                    let m = sema.member(file, index);
                    let head = match m.ty {
                        Some(ty) => format!("{ty} {}", m.name),
                        None => m.name.to_string(),
                    };
                    let params = m
                        .params
                        .iter()
                        .map(|p| {
                            let mut s = String::new();
                            render_param(&mut s, p);
                            s
                        })
                        .collect();
                    let docs = docs_in(sema, file, &m.docs);
                    (
                        head,
                        params,
                        (!docs.is_empty()).then_some(docs),
                        m.params.iter().map(|p| p.name).collect(),
                    )
                }
                Def::Builtin(b, elem) if b.is_callable() => {
                    let head = match b.ty() {
                        Some(ty) => format!("{ty} {}", b.name()),
                        None => b.name().to_owned(),
                    };
                    (head, b.params(elem), Some(b.docs()), Vec::new())
                }
                _ => return None,
            };

        let mut label = format!("{head}(");
        let mut ranges = Vec::new();
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                label.push_str(", ");
            }
            let start = label.len() as u32;
            label.push_str(p);
            ranges.push((start, label.len() as u32));
        }
        label.push(')');

        let active = match named {
            Some(name) => names.iter().position(|&n| n == name).map(|i| i as u32),
            None => Some(commas),
        };
        Some(SignatureHelp {
            label,
            params: ranges,
            active_param: active.filter(|&a| (a as usize) < params.len()),
            documentation: docs,
        })
    })
    .flatten()
}
