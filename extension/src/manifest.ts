import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";

import { ManifestTemplateRequest, RestartRequest } from "./protocol";
import { settings } from "./settings";
import { runningClient } from "./state";
import { updateStatus } from "./statusBar";

/** `papyrus.createManifest`: writes a `papyrus.toml` into the first workspace folder. */
export async function createManifest(): Promise<void> {
  const folder = vscode.workspace.workspaceFolders?.[0];
  const client = runningClient();
  if (!folder || !client) {
    void vscode.window.showErrorMessage("Open a folder to create a papyrus.toml.");
    return;
  }
  const target = vscode.Uri.joinPath(folder.uri, "papyrus.toml");
  if (!fs.existsSync(target.fsPath)) {
    const template = await client.sendRequest(ManifestTemplateRequest);
    await vscode.workspace.fs.writeFile(target, Buffer.from(template, "utf8"));
    ignoreLocalManifest(folder.uri.fsPath);
  }
  await vscode.window.showTextDocument(target);
  // The server picks the manifest up on save; reload now for the new file.
  updateStatus(await client.sendRequest(RestartRequest, { settings: settings() }));
}

/** Adds `papyrus.local.toml` to the `.gitignore` of a git repository. */
function ignoreLocalManifest(folder: string): void {
  if (!fs.existsSync(path.join(folder, ".git"))) {
    return;
  }
  const gitignore = path.join(folder, ".gitignore");
  const current = fs.existsSync(gitignore) ? fs.readFileSync(gitignore, "utf8") : "";
  if (current.split(/\r?\n/).some((l) => l.trim() === "papyrus.local.toml")) {
    return;
  }
  const sep = current === "" || current.endsWith("\n") ? "" : "\n";
  fs.appendFileSync(gitignore, `${sep}# Machine specific papyrus.toml values\npapyrus.local.toml\n`);
}
