import * as vscode from "vscode";
import type { Middleware } from "vscode-languageclient/node";

/**
 * Renaming a script renames its file too (`Old.psc` → `New.psc`). When VS
 * Code applies the rename of a file, the focus goes to the file tree, and the
 * user, who was typing the new name in the editor, has to click back.
 *
 * So, for a moment after a rename of the server, a renamed script gives the
 * focus back to the editor that had it. Files renamed by hand in the tree keep
 * the focus there: they come without a rename of the server.
 */

/** The editor that had the focus at the last rename, and until when it matters. */
let pending: { uri: vscode.Uri; column: vscode.ViewColumn | undefined; until: number } | undefined;

/** How long after a rename its file renames come, at most. */
const WINDOW_MS = 5000;

/** The `provideRenameEdits` middleware of the language client. */
export const provideRenameEdits: Middleware["provideRenameEdits"] = async (
  document,
  position,
  newName,
  token,
  next,
) => {
  const editor = vscode.window.activeTextEditor;
  const edit = await next(document, position, newName, token);
  if (edit && editor) {
    pending = { uri: editor.document.uri, column: editor.viewColumn, until: Date.now() + WINDOW_MS };
  }
  return edit;
};

/** Gives the focus back to the editor of the rename, renamed with its file. */
export function registerRenameFocus(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.workspace.onDidRenameFiles((e) => {
      const rename = pending;
      if (!rename || Date.now() > rename.until) {
        return;
      }
      const scripts = e.files.filter((f) => f.oldUri.fsPath.toLowerCase().endsWith(".psc"));
      if (scripts.length === 0) {
        return;
      }
      pending = undefined;
      const moved = scripts.find((f) => f.oldUri.toString() === rename.uri.toString());
      const uri = moved?.newUri ?? rename.uri;
      // After VS Code is done moving the editor (and the focus).
      setTimeout(() => {
        void vscode.window.showTextDocument(uri, { viewColumn: rename.column, preserveFocus: false });
      }, 100);
    }),
  );
}
