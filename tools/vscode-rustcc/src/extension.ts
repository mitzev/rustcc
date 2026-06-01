// rustcc VS Code extension entry point.
//
// Activates on Rust sources or workspaces containing a
// rust-toolchain.toml. Shells out to the rustcc CLI for the heavy
// lifting (install / doctor / generate bindings) so this extension
// stays stateless — every command is a thin wrapper around a
// terminal invocation.

import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";

let statusBar: vscode.StatusBarItem | undefined;

export function activate(context: vscode.ExtensionContext): void {
  // ---- Status bar ----------------------------------------------
  statusBar = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Right,
    50,
  );
  statusBar.command = "rustcc.showStatus";
  context.subscriptions.push(statusBar);
  refreshStatusBar();

  // Re-evaluate the status bar whenever the workspace's
  // rust-toolchain.toml changes (toolchain channel may flip
  // between stable and rustcc).
  const watcher = vscode.workspace.createFileSystemWatcher(
    "**/rust-toolchain.toml",
  );
  watcher.onDidChange(refreshStatusBar);
  watcher.onDidCreate(refreshStatusBar);
  watcher.onDidDelete(refreshStatusBar);
  context.subscriptions.push(watcher);

  // ---- Commands ------------------------------------------------
  context.subscriptions.push(
    vscode.commands.registerCommand("rustcc.installToolchain", installToolchain),
    vscode.commands.registerCommand("rustcc.doctor", doctor),
    vscode.commands.registerCommand("rustcc.generateBindings", generateBindings),
    vscode.commands.registerCommand("rustcc.showSkips", showSkips),
    vscode.commands.registerCommand("rustcc.showStatus", showStatus),
    vscode.commands.registerCommand("rustcc.installRaFork", () =>
      installRaFork(context),
    ),
  );

  // ---- Diagnostics from bindings.skips.json --------------------
  const skipDiag = vscode.languages.createDiagnosticCollection("rustcc-skips");
  context.subscriptions.push(skipDiag);
  refreshSkipsDiagnostics(skipDiag);
  const skipsWatcher = vscode.workspace.createFileSystemWatcher(
    "**/bindings.skips.json",
  );
  skipsWatcher.onDidChange(() => refreshSkipsDiagnostics(skipDiag));
  skipsWatcher.onDidCreate(() => refreshSkipsDiagnostics(skipDiag));
  skipsWatcher.onDidDelete(() => skipDiag.clear());
  context.subscriptions.push(skipsWatcher);
}

export function deactivate(): void {
  /* no-op — disposables are tracked via context.subscriptions */
}

// --------- Status bar -----------------------------------------

function refreshStatusBar(): void {
  if (!statusBar) return;
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (!folder) {
    statusBar.hide();
    return;
  }
  const toolchainFile = path.join(folder.uri.fsPath, "rust-toolchain.toml");
  let pinned: string | undefined;
  if (fs.existsSync(toolchainFile)) {
    const body = fs.readFileSync(toolchainFile, "utf8");
    const m = body.match(/channel\s*=\s*"([^"]+)"/);
    if (m) pinned = m[1];
  }
  if (pinned === "rustcc") {
    statusBar.text = "$(rocket) rustcc";
    statusBar.tooltip = "rust-toolchain.toml pins channel = \"rustcc\". Click for status.";
  } else if (pinned) {
    statusBar.text = `$(circle-outline) rustcc: ${pinned}`;
    statusBar.tooltip = `rust-toolchain.toml pins ${pinned}, not rustcc. Click for status.`;
  } else {
    statusBar.text = "$(circle-outline) rustcc";
    statusBar.tooltip = "No rust-toolchain.toml. Click for status.";
  }
  statusBar.show();
}

// --------- Commands -------------------------------------------

function rustccCli(): string {
  return vscode.workspace.getConfiguration("rustcc").get<string>("cliPath") ?? "rustcc";
}

function cargoCli(): string {
  return vscode.workspace.getConfiguration("rustcc").get<string>("cargoPath") ?? "cargo";
}

function runInTerminal(name: string, command: string): void {
  let term = vscode.window.terminals.find((t) => t.name === name);
  if (!term) {
    term = vscode.window.createTerminal({ name });
  }
  term.show(true);
  term.sendText(command);
}

async function installToolchain(): Promise<void> {
  const cli = rustccCli();
  const pick = await vscode.window.showQuickPick(
    [
      { label: "Latest", description: "Resolve `latest` against the GitHub releases redirect" },
      { label: "Pinned tag", description: "Specify a release tag (e.g. v1.13.3)" },
    ],
    { placeHolder: "Which version of the rustcc toolchain to install?" },
  );
  if (!pick) return;
  let version = "latest";
  if (pick.label === "Pinned tag") {
    const input = await vscode.window.showInputBox({
      prompt: "Release tag",
      placeHolder: "v1.13.3",
      validateInput: (v) =>
        /^v\d+\.\d+\.\d+/.test(v) ? null : "Expected a tag like v1.13.3",
    });
    if (!input) return;
    version = input;
  }
  runInTerminal("rustcc install", `${cli} install --version ${version}`);
}

async function doctor(): Promise<void> {
  runInTerminal("rustcc doctor", `${rustccCli()} doctor`);
}

async function generateBindings(): Promise<void> {
  // Two paths: if the workspace has a `gen_bindings` bin (FLTK
  // demo / scaffolded project pattern), run it. Otherwise prompt
  // the user for a header path and fall back to a one-shot CLI.
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (!folder) {
    vscode.window.showErrorMessage("rustcc: open a workspace folder first.");
    return;
  }
  const cargoToml = path.join(folder.uri.fsPath, "Cargo.toml");
  if (!fs.existsSync(cargoToml)) {
    vscode.window.showErrorMessage("rustcc: no Cargo.toml in workspace root.");
    return;
  }
  const body = fs.readFileSync(cargoToml, "utf8");
  if (body.includes("gen_bindings")) {
    runInTerminal("rustcc gen_bindings", `${cargoCli()} run --release --bin gen_bindings`);
    return;
  }
  // Fallback: ask the user for a header to bind. The CLI's
  // `bindings` subcommand isn't shipped yet — placeholder.
  const header = await vscode.window.showOpenDialog({
    canSelectMany: false,
    filters: { "C++ headers": ["hpp", "hh", "hxx", "h"] },
    openLabel: "Generate bindings",
  });
  if (!header || header.length === 0) return;
  vscode.window.showInformationMessage(
    `rustcc: scaffolding a gen_bindings binary that calls Build::compile against ${path.basename(header[0].fsPath)} ` +
      `is tracked as a follow-up. For now: cd into your project root and run \`cargo run --release --bin gen_bindings\` ` +
      `if you have one, or copy the gen_bindings.rs from examples/fltk_text_editor.`,
  );
}

async function showSkips(): Promise<void> {
  const skips = collectSkips();
  if (skips.length === 0) {
    vscode.window.showInformationMessage(
      "rustcc: no bindings.skips.json found. Run \"Generate Bindings for Header\" first.",
    );
    return;
  }
  // Group by class for readability.
  const byClass = new Map<string, SkipRecord[]>();
  for (const r of skips) {
    const arr = byClass.get(r.class) ?? [];
    arr.push(r);
    byClass.set(r.class, arr);
  }
  const channel = vscode.window.createOutputChannel("rustcc skips");
  channel.clear();
  channel.appendLine(`${skips.length} skip(s) across ${byClass.size} class(es):`);
  channel.appendLine("");
  for (const [cls, recs] of byClass) {
    channel.appendLine(`${cls}  (${recs.length})`);
    for (const r of recs) {
      channel.appendLine(`  ${r.method}: ${r.reason}`);
    }
    channel.appendLine("");
  }
  channel.show(true);
}

async function showStatus(): Promise<void> {
  runInTerminal("rustcc doctor", `${rustccCli()} doctor`);
}

// --------- RA fork installer ----------------------------------

/**
 * Download the prebuilt rust-analyzer-rustcc binary for the user's
 * host triple and set `rust-analyzer.server.path` automatically.
 *
 * Mirrors the shape of `rustcc install`: shells to curl + tar +
 * shasum (or sha256sum on Linux), drops the binary at
 * `<extensionStorage>/ra/rust-analyzer-<version>`, then writes a
 * workspace-scoped settings update.
 *
 * Asks the user for the version (latest or a pinned tag).
 */
async function installRaFork(
  context: vscode.ExtensionContext,
): Promise<void> {
  const pick = await vscode.window.showQuickPick(
    [
      {
        label: "Latest",
        description: "Resolve `latest` against the GitHub releases redirect",
      },
      {
        label: "Pinned tag",
        description: "Specify a release tag (e.g. v1.13.3)",
      },
    ],
    {
      placeHolder: "Which version of rust-analyzer-rustcc to install?",
    },
  );
  if (!pick) return;
  let version = "latest";
  if (pick.label === "Pinned tag") {
    const input = await vscode.window.showInputBox({
      prompt: "Release tag",
      placeHolder: "v1.13.3",
      validateInput: (v) =>
        /^v\d+\.\d+\.\d+/.test(v) ? null : "Expected a tag like v1.13.3",
    });
    if (!input) return;
    version = input;
  }

  // Use the workspace-scoped storage for the binary cache so a
  // multi-workspace user can override per-project. Fallbacks:
  // globalStorageUri → ~/.vscode/extensions/rustcc.rustcc-tools-*.
  const targetDir = vscode.Uri.joinPath(context.globalStorageUri, "ra");
  await vscode.workspace.fs.createDirectory(targetDir);

  // The terminal-based path mirrors what `rustcc install` does for
  // the toolchain. Each command runs synchronously in the user's
  // shell — no Node-side HTTP / archive deps means the extension
  // bundle stays tiny.
  const triple = await detectHostTriple();
  if (!triple) {
    vscode.window.showErrorMessage(
      "rustcc: could not detect host triple via `rustc -vV`. " +
        "Install rustc + rustup first.",
    );
    return;
  }

  let resolvedTag = version;
  if (version === "latest") {
    // GitHub redirects releases/latest to the actual tag URL.
    // Use a small Node-side fetch to resolve.
    try {
      resolvedTag = await resolveLatestTag();
    } catch (e) {
      vscode.window.showErrorMessage(
        `rustcc: could not resolve latest release tag: ${e}`,
      );
      return;
    }
  }

  const tarball = `rust-analyzer-rustcc-${triple}.tar.xz`;
  const sha = `${tarball}.sha256`;
  const baseUrl = `https://github.com/mitzev/rustcc/releases/download/${resolvedTag}`;
  const targetFs = targetDir.fsPath;

  const cmd = [
    `cd "${targetFs}"`,
    `curl -fsSL -o ${tarball} ${baseUrl}/${tarball}`,
    `curl -fsSL -o ${sha} ${baseUrl}/${sha}`,
    `(shasum -a 256 --check ${sha} || sha256sum --check ${sha})`,
    `tar -xJf ${tarball}`,
    `rm -f ${tarball} ${sha}`,
    `echo`,
    `echo "rust-analyzer-rustcc ${resolvedTag} installed at ${targetFs}/rust-analyzer-rustcc/rust-analyzer"`,
    `echo "VS Code setting rust-analyzer.server.path will be updated automatically."`,
  ].join(" && ");
  runInTerminal("rustcc: install RA fork", cmd);

  // Wait briefly for the user's shell to finish, then point RA at
  // the binary. The binary path is deterministic; if the curl
  // failed, server.path will point at a non-existent file but the
  // user gets a clear error from rust-analyzer.
  const binaryPath = path.join(
    targetFs,
    "rust-analyzer-rustcc",
    "rust-analyzer",
  );
  const cfg = vscode.workspace.getConfiguration("rust-analyzer");
  await cfg.update(
    "server.path",
    binaryPath,
    vscode.ConfigurationTarget.Workspace,
  );

  vscode.window.showInformationMessage(
    `rust-analyzer.server.path set to ${binaryPath}. ` +
      "Reload the window after the download completes.",
  );
}

async function detectHostTriple(): Promise<string | undefined> {
  const { exec } = await import("child_process");
  return new Promise((resolve) => {
    exec("rustc -vV", (err, stdout) => {
      if (err) {
        resolve(undefined);
        return;
      }
      for (const line of stdout.split("\n")) {
        if (line.startsWith("host: ")) {
          resolve(line.slice(6).trim());
          return;
        }
      }
      resolve(undefined);
    });
  });
}

async function resolveLatestTag(): Promise<string> {
  const { exec } = await import("child_process");
  return new Promise((resolve, reject) => {
    exec(
      'curl -fsSLI -o /dev/null -w "%{url_effective}" https://github.com/mitzev/rustcc/releases/latest',
      (err, stdout) => {
        if (err) {
          reject(err);
          return;
        }
        const trimmed = stdout.trim();
        const tag = trimmed.split("/").pop();
        if (!tag || !tag.startsWith("v")) {
          reject(new Error(`unexpected redirect target: ${trimmed}`));
          return;
        }
        resolve(tag);
      },
    );
  });
}

// --------- Diagnostics from bindings.skips.json ---------------

interface SkipRecord {
  class: string;
  method: string;
  reason: string;
}

interface SkipLog {
  schema_version: number;
  skips: SkipRecord[];
}

function collectSkips(): SkipRecord[] {
  const folders = vscode.workspace.workspaceFolders ?? [];
  const out: SkipRecord[] = [];
  for (const f of folders) {
    walk(f.uri.fsPath, (file) => {
      if (path.basename(file) === "bindings.skips.json") {
        try {
          const log = JSON.parse(fs.readFileSync(file, "utf8")) as SkipLog;
          if (log && Array.isArray(log.skips)) out.push(...log.skips);
        } catch {
          /* ignore malformed */
        }
      }
    });
  }
  return out;
}

function walk(root: string, visit: (file: string) => void): void {
  try {
    const entries = fs.readdirSync(root, { withFileTypes: true });
    for (const e of entries) {
      const p = path.join(root, e.name);
      if (e.isDirectory()) {
        if (e.name === "node_modules" || e.name === ".git") continue;
        walk(p, visit);
      } else if (e.isFile()) {
        visit(p);
      }
    }
  } catch {
    /* unreadable dir — skip */
  }
}

function refreshSkipsDiagnostics(coll: vscode.DiagnosticCollection): void {
  coll.clear();
  // The skip records reference C++ classes; we surface them as
  // workspace-wide info diagnostics anchored at any bindings.rs
  // file we find. For the v0 the diagnostic is attached to the
  // first line of bindings.rs — a richer mapping (anchor each
  // skip to the originating class's `impl` block) would need
  // either source-position metadata in the JSON or a re-parse
  // of bindings.rs. Tracked as a v0.2 follow-up.
  const skips = collectSkips();
  if (skips.length === 0) return;

  const folders = vscode.workspace.workspaceFolders ?? [];
  for (const f of folders) {
    walk(f.uri.fsPath, (file) => {
      if (path.basename(file) !== "bindings.rs") return;
      const uri = vscode.Uri.file(file);
      const range = new vscode.Range(0, 0, 0, 0);
      const diagnostics: vscode.Diagnostic[] = skips.map(
        (r) =>
          new vscode.Diagnostic(
            range,
            `${r.class}::${r.method} — ${r.reason}`,
            vscode.DiagnosticSeverity.Information,
          ),
      );
      coll.set(uri, diagnostics);
    });
  }
}
