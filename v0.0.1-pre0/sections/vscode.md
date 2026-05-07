<section class="manual-sheet" id="vscode" markdown="1">

# Visual Studio Code

[Visual Studio Code](https://code.visualstudio.com/) (VS Code) is a free, open-source editor with first-class support for Rust and embedded development. While any editor works, VS Code is recommended for this project.

## Installing VS Code

Download and install VS Code for your platform from [code.visualstudio.com](https://code.visualstudio.com/download).

| Platform | Installation |
|----------|-------------|
| **Linux** | Download the `.deb` / `.rpm` package, or install via your package manager (`sudo apt install code`, `sudo dnf install code`) |
| **macOS** | Download the `.zip`, extract it, and drag **Visual Studio Code** to your **Applications** folder |
| **Windows** | Run the `.exe` installer |

## Opening the Repository

Open the extracted repository folder in VS Code using any of the following methods:

**File menu:** <ui-tab>File</ui-tab> → <ui-menu>Open Folder…</ui-menu>, then navigate to the repository root.

**File Explorer / Finder (macOS):** Drag the repository folder onto the VS Code icon in the Dock or taskbar.

**Terminal:**

```console
code kiwi-firmware-*/
```

VS Code will prompt you to install the recommended extensions — click <ui-btn>Install</ui-btn> to accept.

## Initializing Git Tracking

Because the source code is downloaded as an archive rather than cloned from a repository, it does not come with Git history. Initializing Git tracking locally lets you record your own changes, experiment safely on branches, and revert to a known-good state at any time.

From the repository root, run:

```console
git init
git add .
git commit -m "Initial commit"
```

VS Code's built-in **Source Control** view (<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>G</kbd> / <kbd>⌃⇧G</kbd>) will then show modified files, diffs, and let you stage and commit changes without leaving the editor.

> **Tip:** If you want to push your changes to your own remote (e.g. a private GitHub repository), create the remote first and then add it:
>
> ```console
> git remote add origin https://github.com/YOUR_USERNAME/YOUR_REPO.git
> git push -u origin main
> ```

## Recommended Extensions

Install the following extensions from the [Extensions view](https://code.visualstudio.com/docs/editor/extension-marketplace) (<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>X</kbd> / <kbd>⇧⌘X</kbd>):

| Extension | Purpose |
|-----------|---------|
| [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer) | Full Rust language support: autocomplete, inline diagnostics, go-to-definition, and refactoring |
| [probe-rs-debugger](https://marketplace.visualstudio.com/items?itemName=probe-rs.probe-rs-debugger) | On-chip debugging, breakpoints, and live variable inspection over the debug probe |
| [Even Better TOML](https://marketplace.visualstudio.com/items?itemName=tamasfe.even-better-toml) | Syntax highlighting and validation for `Cargo.toml` files |

## Why VS Code?

- **Inline errors and warnings** — `rust-analyzer` shows compiler diagnostics directly in the editor as you type, without needing to run `cargo build` manually.
- **Autocomplete and documentation** — hover over any symbol to see its type signature and documentation, and get context-aware completions.
- **On-chip debugging** — the `probe-rs-debugger` extension lets you set breakpoints, step through firmware, and inspect variables on the live target via the debug probe, all without leaving the editor.
- **Integrated terminal** — run `cargo run`, flash firmware, and stream RTT logs from a terminal panel inside the editor.
- **Git integration** — stage, commit, and diff changes without switching windows.

</section>
