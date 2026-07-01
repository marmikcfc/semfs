//! `semfs init` — install the grep shell wrapper.

use anyhow::Result;
use clap::Args as ClapArgs;

#[derive(ClapArgs, Debug)]
pub struct Args {}

const SHELL_WRAPPER: &str = r#"
# semfs grep wrapper — semantic search inside mounted containers
grep() {
    # Any flag → real grep; semantic doesn't know about flags.
    for arg in "$@"; do
        case "$arg" in
            -*) command grep "$@"; return ;;
        esac
    done

    _semfs_found=""

    # Path A: CWD walk. Trigger if $PWD is actually inside a mount path.
    _semfs_dir="$PWD"
    _semfs_pwd_real="$(pwd -P)"
    while [ "$_semfs_dir" != "/" ]; do
        if [ -f "$_semfs_dir/.semfs" ]; then
            while IFS= read -r _semfs_mp; do
                _semfs_mp_real="$(cd "$_semfs_mp" 2>/dev/null && pwd -P)"
                case "$_semfs_pwd_real" in "$_semfs_mp_real"|"$_semfs_mp_real"/*) _semfs_found=1; break ;; esac
            done <<_SEMFS_A_EOF
$(command grep '^mount_path=' "$_semfs_dir/.semfs" 2>/dev/null | cut -d= -f2-)
_SEMFS_A_EOF
            break
        fi
        _semfs_dir="$(dirname "$_semfs_dir")"
    done

    # Path B: check path args (skip the first non-flag arg — it's grep's pattern).
    # A match only counts when the resolved path is actually inside the mount.
    if [ -z "$_semfs_found" ]; then
        _semfs_pattern_seen=0
        for arg in "$@"; do
            case "$arg" in -*) continue ;; esac
            if [ "$_semfs_pattern_seen" = "0" ]; then
                _semfs_pattern_seen=1
                continue
            fi
            if [ -d "$arg" ]; then
                _semfs_resolved="$(cd "$arg" 2>/dev/null && pwd -P)"
            elif [ -e "$arg" ] || [ -d "$(dirname "$arg")" ]; then
                _semfs_parent="$(cd "$(dirname "$arg")" 2>/dev/null && pwd -P)"
                [ -z "$_semfs_parent" ] && continue
                _semfs_resolved="$_semfs_parent/$(basename "$arg")"
            else
                continue
            fi
            [ -z "$_semfs_resolved" ] && continue
            _semfs_dir="$_semfs_resolved"
            [ ! -d "$_semfs_dir" ] && _semfs_dir="$(dirname "$_semfs_dir")"
            while [ "$_semfs_dir" != "/" ]; do
                if [ -f "$_semfs_dir/.semfs" ]; then
                    while IFS= read -r _semfs_mp; do
                        _semfs_mp_real="$(cd "$_semfs_mp" 2>/dev/null && pwd -P)"
                        case "$_semfs_resolved" in
                            "$_semfs_mp_real"|"$_semfs_mp_real"/*) _semfs_found=1; break 2 ;;
                        esac
                    done <<_SEMFS_B_EOF
$(command grep '^mount_path=' "$_semfs_dir/.semfs" 2>/dev/null | cut -d= -f2-)
_SEMFS_B_EOF
                    break
                fi
                _semfs_dir="$(dirname "$_semfs_dir")"
            done
        done
    fi

    if [ -n "$_semfs_found" ]; then
        semfs grep "$@"
    else
        command grep "$@"
    fi
}
"#;

const MARKER: &str = "semfs grep wrapper";

/// The shell startup files the wrapper must live in so `grep` resolves to
/// `semfs grep` for EVERY shell an agent (or human) might use:
///   - `~/.zshrc`  — zsh interactive.
///   - `~/.bashrc` — bash *interactive* (e.g. a human terminal tab).
///   - bash LOGIN file — what `bash -lc '…'` sources. Codex/Claude run commands
///     via `bash -lc`, a *non-interactive login* shell. The Ubuntu-default
///     `~/.bashrc` early-returns when non-interactive (`case $- in *i*) ;;
///     *) return;;`), so the wrapper there never runs for an agent. The login
///     file (`~/.bash_profile`|`~/.bash_login`|`~/.profile`) has no such guard,
///     so the wrapper placed there DOES run for `bash -lc` — this is the line
///     that makes a plain `grep` semantic for codex inside a mount.
/// The wrapper body is POSIX `sh` (no bashisms), so it is safe in `~/.profile`.
fn target_rc_files() -> Result<Vec<std::path::PathBuf>> {
    let home = std::env::var("HOME").map(std::path::PathBuf::from)?;
    let mut files = vec![home.join(".zshrc"), home.join(".bashrc")];
    files.push(bash_login_file(&home));
    Ok(files)
}

/// The file a bash *login* shell sources: the first of `~/.bash_profile`,
/// `~/.bash_login`, `~/.profile` that exists. We must NOT create
/// `~/.bash_profile` when only `~/.profile` exists — doing so would shadow
/// `~/.profile` and change the user's login behavior. Default to `~/.profile`
/// (the universal fallback bash reads when no bash-specific login file exists).
fn bash_login_file(home: &std::path::Path) -> std::path::PathBuf {
    for name in [".bash_profile", ".bash_login", ".profile"] {
        let p = home.join(name);
        if p.is_file() {
            return p;
        }
    }
    home.join(".profile")
}

/// Append the wrapper to each target shell file that lacks it. No-op for any
/// file that already has the wrapper — this is the cheap path used by `mount`.
/// Returns true when at least one fresh install happened.
pub fn ensure_grep_wrapper_present() -> Result<bool> {
    let mut installed = false;
    for rc in target_rc_files()? {
        if let Ok(content) = std::fs::read_to_string(&rc) {
            if content.contains(MARKER) {
                continue;
            }
        }
        append_wrapper(&rc)?;
        installed = true;
    }
    Ok(installed)
}

/// Strip any existing wrapper block and append a fresh copy to every target
/// shell file. Force path used by `semfs init` — run after upgrading the binary
/// so the shell integration matches the current version.
pub fn reinstall_grep_wrapper() -> Result<()> {
    for rc in target_rc_files()? {
        if let Ok(content) = std::fs::read_to_string(&rc) {
            if content.contains(MARKER) {
                let mut cleaned = String::new();
                let mut skip = false;
                for line in content.lines() {
                    if line.contains(MARKER) {
                        skip = true;
                        continue;
                    }
                    if skip && line.trim() == "}" {
                        skip = false;
                        continue;
                    }
                    if !skip {
                        cleaned.push_str(line);
                        cleaned.push('\n');
                    }
                }
                std::fs::write(&rc, cleaned)?;
            }
        }
        append_wrapper(&rc)?;
    }
    Ok(())
}

fn append_wrapper(rc: &std::path::Path) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(rc)?;
    file.write_all(SHELL_WRAPPER.as_bytes())?;
    Ok(())
}

pub async fn run(_args: Args) -> Result<()> {
    reinstall_grep_wrapper()?;
    eprintln!("semantic grep (re)installed for zsh + bash (interactive and login shells).");
    eprintln!("open a new terminal, or `source` your shell rc to use it now.");
    Ok(())
}
