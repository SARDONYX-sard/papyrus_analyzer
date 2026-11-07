import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";

import { compileEvents, runCompile } from "./compile/task";
import { client, showAssemblyCommand } from "./decompile";
import { isBelow, isWithin, key } from "./paths";
import { DecompileFilesRequest } from "./protocol";
import { scriptScope, state } from "./state";
import { runTests } from "./tests";

/**
 * The Papyrus explorer: the status of the language server (click: details,
 * button: restart), then the `.psc` and `.pex` files: the workspace folders,
 * the sources of papyrus.toml and the outputs, even below ignored directories
 * such as `target`. The mode, switched by a click on the mode row, chooses
 * which files the view acts on:
 *
 * - compile: the `.psc` files, decompiled ones excepted;
 * - decompile: the `.pex` files, compiled ones included.
 *
 * The other files stay visible, dimmed like ignored files, and open like the others;
 * files written since the last run get a dot. Below the mode, the compiler
 * (compile mode) and the output directory of the mode.
 *
 * A click opens a file; the ▶ button of a file or folder runs the
 * action on it (everything below, for a folder). In compile mode, the test
 * scripts (`[test] sources`, never compiled) get a Run Tests button instead:
 * `papyrus_analyzer test` on them. Each script shows the
 * result of its last run. Outputs keep the directory structure:
 * `[build] output` of papyrus.toml (`decompile` below it), else
 * `build/papyrus/compile` and `build/papyrus/decompile` of the workspace
 * folder.
 */

type Mode = "compile" | "decompile";
type Status = "running" | "passed" | "failed";

const MODE_KEY = "papyrus.explorerView.mode";
/** The scheme of the rows, so that their decorations stay in this view. */
const SCHEME = "papyrus-explorer";

interface FolderNode {
  kind: "folder";
  uri: vscode.Uri;
  name: string;
  children: (FolderNode | FileNode)[];
}

interface FileNode {
  kind: "file";
  uri: vscode.Uri;
}

/** The first row: the status of the language server. */
interface ServerNode {
  kind: "server";
}

/** The current mode, in capitals; a click switches it. */
interface ModeNode {
  kind: "mode";
}

/** In compile mode: the compiler used, built-in or `[compiler] path`. */
interface CompilerNode {
  kind: "compiler";
}

/** Where the mode writes its files; a click reveals the directory. */
interface OutputNode {
  kind: "output";
}

type Node =
  | ServerNode
  | ModeNode
  | CompilerNode
  | OutputNode
  | FolderNode
  | FileNode;

/** The one server row, so that a status update redraws only it. */
const SERVER: ServerNode = { kind: "server" };

/** The row URI of a file: same path, own scheme. */
function rowUri(uri: vscode.Uri): vscode.Uri {
  return uri.with({ scheme: SCHEME });
}

class ExplorerProvider
  implements vscode.TreeDataProvider<Node>, vscode.FileDecorationProvider
{
  private readonly changed = new vscode.EventEmitter<Node | undefined>();
  readonly onDidChangeTreeData = this.changed.event;
  private readonly decorationsChanged = new vscode.EventEmitter<
    vscode.Uri | vscode.Uri[] | undefined
  >();
  readonly onDidChangeFileDecorations = this.decorationsChanged.event;
  mode: Mode;
  private roots: Node[] = [];
  private loaded = false;
  private readonly statuses = new Map<string, Status>();
  private readonly errors = new Map<string, string>();
  /** Files written since the last run started, by path key. */
  private readonly fresh = new Set<string>();
  /** When the last run started (ms since 1970), 0 before the first one. */
  private runStarted = 0;
  /** The file nodes by path key, to refresh one row when its status changes. */
  private nodes = new Map<string, FileNode>();
  /**
   * Folders expanded or collapsed with a button, by path key. VS Code keeps
   * the state of the ids it knows, so the generation, part of the id, is
   * bumped to apply `expanded`.
   */
  private readonly folders = new Map<
    string,
    { generation: number; expanded: boolean }
  >();

  constructor(private readonly memento: vscode.Memento) {
    this.mode = memento.get<Mode>(MODE_KEY, "compile");
  }

  async setMode(mode: Mode): Promise<void> {
    this.mode = mode;
    await this.memento.update(MODE_KEY, mode);
    // Same files, other targets.
    this.changed.fire(undefined);
    this.decorationsChanged.fire(undefined);
  }

  refresh(): void {
    this.loaded = false;
    this.docs.clear();
    this.changed.fire(undefined);
    this.decorationsChanged.fire(undefined);
  }

  setStatus(file: string, status: Status, error?: string): void {
    this.statuses.set(key(file), status);
    if (error) {
      this.errors.set(key(file), error);
    } else {
      this.errors.delete(key(file));
    }
    // Only the row of the file: redrawing thousands of rows for each script
    // compiled keeps the extension host busy.
    const node = this.nodes.get(key(file));
    if (node) {
      this.changed.fire(node);
    }
  }

  /** Expands or collapses `node` and every folder below it; every folder without node. */
  setExpanded(node: Node | undefined, expanded: boolean): void {
    const visit = (n: Node) => {
      if (n.kind !== "folder") {
        return;
      }
      const k = key(n.uri.fsPath);
      const generation = (this.folders.get(k)?.generation ?? 0) + 1;
      this.folders.set(k, { generation, expanded });
      n.children.forEach(visit);
    };
    (node ? [node] : this.roots).forEach(visit);
    this.changed.fire(undefined);
  }

  /** A file was written on disk. */
  markFresh(uri: vscode.Uri): void {
    this.fresh.add(key(uri.fsPath));
    this.decorationsChanged.fire(rowUri(uri));
  }

  /** A run starts: earlier outputs are not new anymore. */
  startRun(): void {
    this.runStarted = Date.now();
    if (this.fresh.size > 0) {
      this.fresh.clear();
      this.decorationsChanged.fire(undefined);
    }
  }

  /** The directories that get compiled or decompiled files. */
  private buildDirs(): string[] {
    const folders = (vscode.workspace.workspaceFolders ?? []).map(
      (f) => f.uri.fsPath,
    );
    const dirs = [
      ...state.buildDirs.flatMap((b) => [b.compile, b.decompile]),
      ...folders.map((f) => path.join(f, "build", "papyrus")),
    ];
    // Without the ones inside another (`output/decompile` in `output`).
    return dirs.filter((d) => !dirs.some((o) => isBelow(d, o)));
  }

  /** The output directories of the current mode. */
  outputDirs(): string[] {
    const dirs = state.buildDirs.map((b) =>
      this.mode === "compile" ? b.compile : b.decompile,
    );
    return [...new Map(dirs.map((d) => [key(d), d])).values()];
  }

  /**
   * Whether the current mode acts on `file`: the `.psc` files but the
   * decompiled ones (compile), or the `.pex` files, compiled ones included
   * (decompile). The others are shown dimmed.
   */
  isTarget(file: string): boolean {
    if (this.mode === "compile") {
      return (
        file.toLowerCase().endsWith(".psc") &&
        !this.buildDirs().some((dir) => isBelow(file, dir)) &&
        !this.isTest(file)
      );
    }
    const decompiled = state.buildDirs.map((b) => b.decompile);
    return (
      file.toLowerCase().endsWith(".pex") &&
      !decompiled.some((dir) => isWithin(file, dir))
    );
  }

  /**
   * Whether `file` is a test script: a `.psc` file of `[test] sources`, never
   * compiled; in compile mode, its button runs its tests instead.
   */
  isTest(file: string): boolean {
    return (
      this.mode === "compile" &&
      file.toLowerCase().endsWith(".psc") &&
      state.testSources.some((dir) => isWithin(file, dir))
    );
  }

  /** Whether the documentation of the `.psc` file has examples to test, by path key. */
  private readonly docs = new Map<string, boolean>();

  /** Whether `file` has doc tests: a ```` ```papyrus ```` example not `ignore`d. */
  hasDocTests(file: string): boolean {
    if (this.mode !== "compile" || !file.toLowerCase().endsWith(".psc")) {
      return false;
    }
    const k = key(file);
    let has = this.docs.get(k);
    if (has === undefined) {
      try {
        has = /```papyrus(?![^\n]*ignore)/i.test(fs.readFileSync(file, "utf8"));
      } catch {
        has = false;
      }
      this.docs.set(k, has);
    }
    return has;
  }

  /** `file` changed: its row gets or loses the doc test button. */
  docsChanged(uri: vscode.Uri): void {
    const k = key(uri.fsPath);
    const had = this.docs.get(k);
    this.docs.delete(k);
    const node = this.nodes.get(k);
    if (had !== undefined && node && this.hasDocTests(uri.fsPath) !== had) {
      this.changed.fire(node);
    }
  }

  /** The `.psc` files of `node` with doc tests. */
  docTests(node: Node): vscode.Uri[] {
    switch (node.kind) {
      case "file":
        return this.hasDocTests(node.uri.fsPath) ? [node.uri] : [];
      case "folder":
        return node.children.flatMap((c) => this.docTests(c));
      default:
        return [];
    }
  }

  /** The test scripts of `node`. */
  tests(node: Node): vscode.Uri[] {
    switch (node.kind) {
      case "file":
        return this.isTest(node.uri.fsPath) ? [node.uri] : [];
      case "folder":
        return node.children.flatMap((c) => this.tests(c));
      default:
        return [];
    }
  }

  /** The roots of the tree: the workspace folders, plus the sources and outputs outside of them. */
  private rootDirs(): string[] {
    const roots: string[] = [];
    const folders = (vscode.workspace.workspaceFolders ?? []).map(
      (f) => f.uri.fsPath,
    );
    for (const dir of [...folders, ...state.sources, ...this.buildDirs()]) {
      if (!roots.some((r) => isWithin(dir, r))) {
        roots.push(dir);
      }
    }
    return roots;
  }

  private async load(): Promise<void> {
    const include = this.rootDirs();
    const searched = [...include, ...state.sources].filter(
      (dir, i, all) => all.findIndex((d) => key(d) === key(dir)) === i,
    );
    const found = await Promise.all(
      searched.map((dir) =>
        vscode.workspace.findFiles(
          new vscode.RelativePattern(vscode.Uri.file(dir), "**/*.{psc,pex}"),
          "**/{node_modules,target,.git}/**",
        ),
      ),
    );
    // The outputs are read from the disk: they are often below an excluded
    // directory (`${workspaceFolder}/target/papyrus/output`), which neither
    // the search nor the file watcher see. Files newer than the last run are
    // its outputs.
    const outputs: { uri: vscode.Uri; mtime: number }[] = [];
    await Promise.all(this.buildDirs().map((dir) => walk(dir, outputs)));
    for (const o of outputs) {
      if (this.runStarted > 0 && o.mtime >= this.runStarted) {
        this.fresh.add(key(o.uri.fsPath));
      }
    }
    found.push(outputs.map((o) => o.uri));
    const roots: FolderNode[] = include.map((dir) => ({
      kind: "folder",
      uri: vscode.Uri.file(dir),
      name: path.basename(dir),
      children: [],
    }));
    const nodes = new Map<string, FileNode>();
    for (const file of found.flat()) {
      const k = key(file.fsPath);
      // Under the root containing it, whichever search found it.
      const root = roots.find((r) => isWithin(file.fsPath, r.uri.fsPath));
      if (root && !nodes.has(k)) {
        nodes.set(k, insert(root, file));
      }
    }
    this.nodes = nodes;
    for (const root of roots) {
      sort(root);
    }
    // The folders of new outputs open, so that they show up after a run.
    for (const o of outputs) {
      const root = roots.find((r) => isWithin(o.uri.fsPath, r.uri.fsPath));
      if (!root || !this.fresh.has(key(o.uri.fsPath))) {
        continue;
      }
      for (
        let dir = path.dirname(o.uri.fsPath);
        isWithin(dir, root.uri.fsPath);
        dir = path.dirname(dir)
      ) {
        const k = key(dir);
        if (!this.folders.get(k)?.expanded) {
          this.folders.set(k, {
            generation: (this.folders.get(k)?.generation ?? 0) + 1,
            expanded: true,
          });
        }
        if (k === key(root.uri.fsPath)) {
          break;
        }
      }
    }
    const nonEmpty = roots.filter((r) => r.children.length > 0);
    // One folder: show its content directly.
    this.roots = nonEmpty.length === 1 ? nonEmpty[0].children : nonEmpty;
    this.loaded = true;
  }

  async getChildren(node?: Node): Promise<Node[]> {
    if (!node) {
      if (!this.loaded) {
        await this.load();
      }
      const header: Node[] =
        this.mode === "compile"
          ? [SERVER, { kind: "mode" }, { kind: "compiler" }, { kind: "output" }]
          : [SERVER, { kind: "mode" }, { kind: "output" }];
      return [...header, ...this.roots];
    }
    return node.kind === "folder" ? node.children : [];
  }

  /** The files of `node` the current mode acts on. */
  targets(node: Node): vscode.Uri[] {
    switch (node.kind) {
      case "file":
        return this.isTarget(node.uri.fsPath) ? [node.uri] : [];
      case "folder":
        return node.children.flatMap((c) => this.targets(c));
      default:
        return [];
    }
  }

  provideFileDecoration(uri: vscode.Uri): vscode.FileDecoration | undefined {
    if (uri.scheme !== SCHEME) {
      return undefined;
    }
    const file = uri.with({ scheme: "file" });
    const node = this.nodes.get(key(file.fsPath));
    const folder = !node;
    const active = folder
      ? this.folderHasTargets(file.fsPath)
      : this.isTarget(file.fsPath) || this.isTest(file.fsPath);
    const fresh = !folder && this.fresh.has(key(file.fsPath));
    const limited = !folder && this.isLimited(file.fsPath);
    if (active && !fresh && !limited) {
      return undefined;
    }
    return {
      badge: fresh ? "•" : limited ? "L" : undefined,
      tooltip: fresh
        ? "Written since the last run"
        : limited
          ? "Limited: in neither [build] sources nor [test] sources of papyrus.toml"
          : undefined,
      color: !active
        ? new vscode.ThemeColor("gitDecoration.ignoredResourceForeground")
        : limited
          ? new vscode.ThemeColor("charts.blue")
          : undefined,
    };
  }

  /**
   * Whether `file` is a script outside of the projects of papyrus.toml.
   * Without papyrus.toml every script is, which the server row says.
   */
  private isLimited(file: string): boolean {
    return (
      state.hasManifest &&
      file.toLowerCase().endsWith(".psc") &&
      !this.buildDirs().some((dir) => isBelow(file, dir)) &&
      scriptScope(file) === "limited"
    );
  }

  private folderHasTargets(dir: string): boolean {
    for (const node of this.nodes.values()) {
      const file = node.uri.fsPath;
      if (isBelow(file, dir) && (this.isTarget(file) || this.isTest(file))) {
        return true;
      }
    }
    return false;
  }

  /** Redraws the server row. */
  refreshServer(): void {
    this.changed.fire(SERVER);
  }

  getTreeItem(node: Node): vscode.TreeItem {
    if (node.kind === "server") {
      return serverItem();
    }
    if (node.kind === "mode") {
      const compile = this.mode === "compile";
      const item = new vscode.TreeItem(
        compile ? "COMPILE MODE" : "DECOMPILE MODE",
      );
      item.contextValue = "papyrusMode";
      item.description = compile
        ? ".psc → .pex  (click: switch to decompile)"
        : ".pex → .psc  (click: switch to compile)";
      item.iconPath = new vscode.ThemeIcon(
        compile ? "file-code" : "file-binary",
        new vscode.ThemeColor(compile ? "charts.green" : "charts.purple"),
      );
      item.tooltip = compile
        ? "Compile mode: ▶ compiles the .psc sources. Click here to switch to decompile mode."
        : "Decompile mode: ▶ decompiles the .pex files. Click here to switch to compile mode.";
      item.command = {
        command: compile
          ? "papyrus.explorer.decompileMode"
          : "papyrus.explorer.compileMode",
        title: "Switch mode",
      };
      return item;
    }
    if (node.kind === "compiler") {
      return compilerItem();
    }
    if (node.kind === "output") {
      return this.outputItem();
    }
    if (node.kind === "folder") {
      // Collapsed until expanded: a workspace may hold thousands of scripts.
      const k = key(node.uri.fsPath);
      const folder = this.folders.get(k);
      const item = new vscode.TreeItem(
        node.name,
        folder?.expanded
          ? vscode.TreeItemCollapsibleState.Expanded
          : vscode.TreeItemCollapsibleState.Collapsed,
      );
      item.id = `${folder?.generation ?? 0}:${k}`;
      item.resourceUri = rowUri(node.uri);
      // A folder of scripts compiles them; a folder of tests only runs them.
      item.contextValue =
        this.targets(node).length > 0
          ? "papyrusFolder"
          : this.tests(node).length > 0
            ? "papyrusFolderTests"
            : "papyrusFolderOther";
      // The directories of the projects stand out from the limited ones.
      const sources = state.sources.some((d) => key(d) === k);
      const tests = state.testSources.some((d) => key(d) === k);
      item.iconPath =
        sources || tests
          ? new vscode.ThemeIcon("root-folder")
          : vscode.ThemeIcon.Folder;
      item.description = sources
        ? "[build] sources"
        : tests
          ? "[test] sources"
          : undefined;
      item.tooltip = node.uri.fsPath;
      return item;
    }
    const item = new vscode.TreeItem(
      rowUri(node.uri),
      vscode.TreeItemCollapsibleState.None,
    );
    // `Doc`: examples in the documentation, a button runs them.
    item.contextValue =
      (this.isTarget(node.uri.fsPath)
        ? "papyrusFile"
        : this.isTest(node.uri.fsPath)
          ? "papyrusFileTest"
          : "papyrusFileOther") + (this.hasDocTests(node.uri.fsPath) ? "Doc" : "");
    const status = this.statuses.get(key(node.uri.fsPath));
    const error = this.errors.get(key(node.uri.fsPath));
    if (status === "running") {
      item.iconPath = new vscode.ThemeIcon("loading~spin");
    } else if (status === "passed") {
      item.iconPath = new vscode.ThemeIcon(
        "pass",
        new vscode.ThemeColor("testing.iconPassed"),
      );
    } else if (status === "failed") {
      item.iconPath = new vscode.ThemeIcon(
        "error",
        new vscode.ThemeColor("testing.iconFailed"),
      );
    } else {
      item.iconPath = vscode.ThemeIcon.File;
    }
    item.tooltip = error ?? node.uri.fsPath;
    item.command = {
      command: "papyrus.explorer.open",
      title: "Open",
      arguments: [node],
    };
    return item;
  }

  private outputItem(): vscode.TreeItem {
    const dirs = this.outputDirs();
    const folders = (vscode.workspace.workspaceFolders ?? []).map(
      (f) => f.uri.fsPath,
    );
    const show = (dir: string) => {
      const folder = folders.find((f) => isWithin(dir, f));
      return folder ? path.relative(folder, dir) || "." : dir;
    };
    const item = new vscode.TreeItem(
      `Output: ${dirs.length === 1 ? show(dirs[0]) : dirs.length === 0 ? "—" : `${dirs.length} folders`}`,
    );
    item.contextValue = "papyrusOutput";
    item.iconPath = new vscode.ThemeIcon("folder-opened");
    item.tooltip =
      dirs.length > 0
        ? `${dirs.join("\n")}\n\nClick to reveal.`
        : "No output directory yet.";
    if (dirs.length > 0) {
      item.command = {
        command: "revealFileInOS",
        title: "Reveal",
        arguments: [vscode.Uri.file(dirs[0])],
      };
    }
    return item;
  }
}

/** The server row: what the status bar shows while the server works. */
function serverItem(): vscode.TreeItem {
  const { label, icon, color, tooltip } = state.server;
  const item = new vscode.TreeItem(label);
  item.contextValue = "papyrusServer";
  item.iconPath = new vscode.ThemeIcon(
    icon,
    color ? new vscode.ThemeColor(color) : undefined,
  );
  item.tooltip = new vscode.MarkdownString(tooltip, true);
  item.command = { command: "papyrus.showStatus", title: "Show Status" };
  return item;
}

/** The compiler row: the one of every project, or `mixed` when they differ. */
function compilerItem(): vscode.TreeItem {
  const builtin = "built-in";
  const compilers = [
    ...new Set(state.buildDirs.map((b) => b.compiler ?? builtin)),
  ];
  const only = compilers.length === 1 ? compilers[0] : undefined;
  const label =
    compilers.length === 0
      ? builtin
      : only === undefined
        ? "mixed"
        : only === builtin
          ? builtin
          : path.basename(only);
  const item = new vscode.TreeItem(`Compiler: ${label}`);
  item.contextValue = "papyrusCompiler";
  item.iconPath = new vscode.ThemeIcon(
    only === builtin || compilers.length === 0 ? "package" : "tools",
  );
  if (only !== undefined && only !== builtin) {
    item.description = path.dirname(only);
  } else if (only === builtin || compilers.length === 0) {
    item.description = state.hasManifest
      ? "no [compiler] path"
      : "no papyrus.toml";
  }
  const lines = state.buildDirs.map(
    (b) => `${b.root}: ${b.compiler ?? builtin}`,
  );
  item.tooltip =
    lines.length > 0
      ? `${lines.join("\n")}\n\nSet [compiler] path in papyrus.toml to use PapyrusCompiler.exe.`
      : "The built-in compiler of papyrus_analyzer.";
  return item;
}

/** The `.psc` and `.pex` files below `dir`, with their modification time. */
async function walk(
  dir: string,
  out: { uri: vscode.Uri; mtime: number }[],
): Promise<void> {
  let entries: fs.Dirent[];
  try {
    entries = await fs.promises.readdir(dir, { withFileTypes: true });
  } catch {
    return;
  }
  await Promise.all(
    entries.map(async (e) => {
      const p = path.join(dir, e.name);
      if (e.isDirectory()) {
        if (e.name !== "node_modules" && !e.name.startsWith(".")) {
          await walk(p, out);
        }
      } else if (/\.(psc|pex)$/i.test(e.name)) {
        const stat = await fs.promises.stat(p).catch(() => undefined);
        out.push({ uri: vscode.Uri.file(p), mtime: stat?.mtimeMs ?? 0 });
      }
    }),
  );
}

function insert(root: FolderNode, file: vscode.Uri): FileNode {
  const parts = path.relative(root.uri.fsPath, file.fsPath).split(path.sep);
  let folder = root;
  for (const part of parts.slice(0, -1)) {
    let next = folder.children.find(
      (c): c is FolderNode => c.kind === "folder" && c.name === part,
    );
    if (!next) {
      next = {
        kind: "folder",
        uri: vscode.Uri.joinPath(folder.uri, part),
        name: part,
        children: [],
      };
      folder.children.push(next);
    }
    folder = next;
  }
  const node: FileNode = { kind: "file", uri: file };
  folder.children.push(node);
  return node;
}

/** Folders first, then by name. */
function sort(folder: FolderNode): void {
  const name = (n: FolderNode | FileNode) =>
    (n.kind === "folder" ? n.name : path.basename(n.uri.fsPath)).toLowerCase();
  folder.children.sort((a, b) =>
    a.kind === b.kind
      ? name(a).localeCompare(name(b))
      : a.kind === "folder"
        ? -1
        : 1,
  );
  for (const child of folder.children) {
    if (child.kind === "folder") {
      sort(child);
    }
  }
}

/** Opens a script, or the decompiled source of a `.pex` file. */
function open(uri: vscode.Uri): void {
  if (uri.fsPath.toLowerCase().endsWith(".pex")) {
    void vscode.commands.executeCommand("papyrus.decompile", uri);
  } else {
    void vscode.window.showTextDocument(uri);
  }
}

/**
 * Shows the view (`papyrus.explorer.show` = `auto`) once the workspace has
 * Papyrus: a papyrus.toml, an open script, or a `.psc` / `.pex` file. Never
 * hidden again in the session.
 */
function watchForScripts(context: vscode.ExtensionContext): void {
  let shown = false;
  const show = () => {
    if (!shown) {
      shown = true;
      void vscode.commands.executeCommand(
        "setContext",
        "papyrus.hasScripts",
        true,
      );
    }
  };
  const isScript = (doc: vscode.TextDocument) => doc.languageId === "papyrus";
  if (state.hasManifest || vscode.workspace.textDocuments.some(isScript)) {
    show();
  } else {
    // One file is enough: the search stops at the first.
    void vscode.workspace
      .findFiles("**/*.{psc,pex}", "**/{node_modules,target,.git}/**", 1)
      .then((found) => found.length > 0 && show());
  }
  state.statusListeners.push(() => state.hasManifest && show());
  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument((doc) => isScript(doc) && show()),
  );
}

/** Registers the view and its commands. */
export function registerExplorerView(context: vscode.ExtensionContext): void {
  watchForScripts(context);
  const provider = new ExplorerProvider(context.workspaceState);
  const view = vscode.window.createTreeView("papyrus.explorer", {
    treeDataProvider: provider,
    showCollapseAll: true,
  });
  const updateMode = () => {
    // Next to the title, the most visible text of the view.
    view.description =
      provider.mode === "compile" ? "Compile mode" : "Decompile mode";
    void vscode.commands.executeCommand(
      "setContext",
      "papyrus.explorerMode",
      provider.mode,
    );
  };
  updateMode();

  const decompile = async (uris: vscode.Uri[], open: boolean) => {
    const c = await client();
    if (!c || uris.length === 0) {
      return;
    }
    for (const uri of uris) {
      provider.setStatus(uri.fsPath, "running");
    }
    const result = await c.sendRequest(DecompileFilesRequest, {
      files: uris.map((u) => u.toString()),
    });
    let failed = 0;
    for (const r of result.results) {
      const file = vscode.Uri.parse(r.file).fsPath;
      provider.setStatus(
        file,
        r.error ? "failed" : "passed",
        r.error ?? undefined,
      );
      if (r.error) {
        failed += 1;
        state.output?.error(r.error);
      }
    }
    const written = result.results.find((r) => r.psc)?.psc;
    if (open && written) {
      await vscode.window.showTextDocument(vscode.Uri.file(written), {
        preview: true,
      });
    } else if (!open) {
      const message = `Decompiled ${result.results.length - failed} scripts into ${result.output}`;
      const reveal = "Reveal";
      const choice = await (failed > 0
        ? vscode.window.showWarningMessage(
            `${message} (${failed} failed, see the output)`,
            reveal,
          )
        : vscode.window.showInformationMessage(message, reveal));
      if (choice === reveal) {
        await vscode.commands.executeCommand(
          "revealFileInOS",
          vscode.Uri.file(result.output),
        );
      }
    }
  };

  const run = async (uris: vscode.Uri[], single: boolean) => {
    if (uris.length === 0) {
      return;
    }
    provider.startRun();
    if (provider.mode === "compile") {
      await runCompile({ type: "papyrus", files: uris.map((u) => u.fsPath) });
    } else {
      await decompile(uris, single);
      // The outputs show up, whatever the watcher sees.
      provider.refresh();
    }
  };

  const watcher = vscode.workspace.createFileSystemWatcher("**/*.{psc,pex}");
  let timer: NodeJS.Timeout | undefined;
  const refreshSoon = () => {
    clearTimeout(timer);
    timer = setTimeout(() => provider.refresh(), 300);
  };
  state.statusListeners.push(refreshSoon);
  state.serverListeners.push(() => provider.refreshServer());

  context.subscriptions.push(
    view,
    watcher,
    vscode.window.registerFileDecorationProvider(provider),
    watcher.onDidCreate((uri) => {
      provider.markFresh(uri);
      refreshSoon();
    }),
    watcher.onDidChange((uri) => {
      provider.markFresh(uri);
      provider.docsChanged(uri);
    }),
    watcher.onDidDelete(refreshSoon),
    compileEvents.event((e) => {
      provider.setStatus(e.script, e.status);
      // A script compiled: its `.pex` shows up, whatever the watcher sees.
      if (e.status !== "running") {
        refreshSoon();
      }
    }),
    vscode.commands.registerCommand(
      "papyrus.explorer.compileMode",
      async () => {
        await provider.setMode("compile");
        updateMode();
      },
    ),
    vscode.commands.registerCommand(
      "papyrus.explorer.decompileMode",
      async () => {
        await provider.setMode("decompile");
        updateMode();
      },
    ),
    vscode.commands.registerCommand("papyrus.explorer.refresh", () =>
      provider.refresh(),
    ),
    vscode.commands.registerCommand("papyrus.explorer.expandAll", () =>
      provider.setExpanded(undefined, true),
    ),
    vscode.commands.registerCommand(
      "papyrus.explorer.expandFolder",
      (node: Node) => provider.setExpanded(node, true),
    ),
    vscode.commands.registerCommand(
      "papyrus.explorer.collapseFolder",
      (node: Node) => provider.setExpanded(node, false),
    ),
    // One command per mode, so that buttons and menus say what they do; both
    // run the action of the current mode, which only shows its own.
    ...["papyrus.explorer.compile", "papyrus.explorer.decompile"].map((id) =>
      vscode.commands.registerCommand(id, (node: Node) =>
        run(provider.targets(node), node.kind === "file"),
      ),
    ),
    ...["papyrus.explorer.compileAll", "papyrus.explorer.decompileAll"].map(
      (id) =>
        vscode.commands.registerCommand(id, async () => {
          const roots = await provider.getChildren();
          await run(
            roots.flatMap((r) => provider.targets(r)),
            false,
          );
        }),
    ),
    // Test scripts: `papyrus_analyzer test` on them, in a terminal.
    vscode.commands.registerCommand("papyrus.explorer.runTests", (node: Node) =>
      runTests(provider.tests(node).map((u) => u.fsPath)),
    ),
    // Scripts with examples in their documentation: `papyrus_analyzer test --doc` on them.
    vscode.commands.registerCommand("papyrus.explorer.runDocTests", async (node: Node) => {
      const files = provider.docTests(node).map((u) => u.fsPath);
      if (files.length === 0) {
        void vscode.window.showInformationMessage("Papyrus: no doc tests here.");
        return;
      }
      await runTests(files, true);
    }),
    vscode.commands.registerCommand("papyrus.explorer.open", (node: Node) => {
      if (node.kind === "file") {
        open(node.uri);
      }
    }),
    // The assembly of a `.pex`, or of a `.psc` compiled in memory.
    vscode.commands.registerCommand(
      "papyrus.explorer.assemble",
      (node: Node) => {
        if (node.kind === "file") {
          void showAssemblyCommand(node.uri);
        }
      },
    ),
  );
}
