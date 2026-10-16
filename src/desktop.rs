//! `.desktop` entry scanning for `--run` (program launcher) mode.

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub struct App {
    pub name: String,
    pub exec: String,
    /// `.desktop` `Keywords=`, matched but never displayed.
    pub keywords: String,
}

const CHECK_KEYS: &[&str] = &[
    "Name",
    "Exec",
    "NoDisplay",
    "Hidden",
    "OnlyShowIn",
    "NotShowIn",
    "Terminal",
    "Keywords",
    "TryExec",
];

/// All application directories per the freedesktop spec, lowest to highest priority.
pub fn app_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(h) = std::env::var("XDG_DATA_HOME") {
        v.push(PathBuf::from(h).join("applications"));
    } else if let Ok(h) = std::env::var("HOME") {
        v.push(PathBuf::from(&h).join(".local/share/applications"));
    }
    if let Ok(d) = std::env::var("XDG_DATA_DIRS") {
        for p in d.split(':') {
            if !p.is_empty() {
                v.push(PathBuf::from(p).join("applications"));
            }
        }
    }
    v.push(PathBuf::from("/usr/local/share/applications"));
    v.push(PathBuf::from("/usr/share/applications"));
    v
}

/// Scan desktop dirs, first-priority wins per file name (freedesktop dedup rule).
pub fn load_apps() -> Vec<App> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut apps = Vec::new();
    for dir in app_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Some(file) = path
                .file_name()
                .and_then(|f| f.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            if !seen.insert(file) {
                continue;
            }
            if let Some(app) = parse_desktop(&path) {
                apps.push(app);
            }
        }
    }
    apps.sort_by_cached_key(|a| a.name.to_lowercase());
    apps
}

fn parse_desktop(path: &Path) -> Option<App> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut in_entry = false;
    let mut name = None;
    let mut localized: Vec<(String, String)> = Vec::new();
    let mut exec = None;
    let mut no_display = false;
    let mut hidden = false;
    let mut terminal = false;
    let mut only_show = None;
    let mut not_show = None;
    let mut keywords = String::new();
    let mut try_exec = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // `Name[zh_CN]` and friends: the user's locale picks among them.
        if let Some(tag) = key.strip_prefix("Name[").and_then(|k| k.strip_suffix(']')) {
            localized.push((tag.to_string(), value.to_string()));
            continue;
        }
        if !CHECK_KEYS.contains(&key) {
            continue;
        }
        match key {
            "Name" => name = Some(value.to_string()),
            "Exec" => exec = Some(value.to_string()),
            "NoDisplay" => no_display = value == "true",
            "Hidden" => hidden = value == "true",
            "Terminal" => terminal = value == "true",
            "OnlyShowIn" => only_show = Some(value.to_string()),
            "NotShowIn" => not_show = Some(value.to_string()),
            // `;`-separated; keep the separators so "web browser" does not
            // match across two keywords.
            "Keywords" => keywords = value.to_lowercase(),
            "TryExec" => try_exec = Some(value.to_string()),
            _ => {}
        }
    }
    // `Terminal=true` cannot work here (there is no terminal to hand it), and a
    // dead entry is worse than no entry.
    if no_display || hidden || terminal {
        return None;
    }
    let current = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if !shown_on(only_show.as_deref(), not_show.as_deref(), &current) {
        return None;
    }
    // `TryExec` is the spec's "is this actually installed?" check.
    if let Some(t) = &try_exec
        && !try_exec_ok(t)
    {
        return None;
    }
    let name = pick_name(&locale_tags(), &localized, name)?;
    let exec = clean_exec(exec.as_deref()?, &name, &path.display().to_string());
    Some(App {
        name,
        exec,
        keywords,
    })
}

/// The user's locale tags, most specific first (`zh_CN.UTF-8` -> `zh_CN`, `zh`).
fn locale_tags() -> Vec<String> {
    let raw = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    let base = raw.split('.').next().unwrap_or("").to_string();
    let lang = base.split('_').next().unwrap_or("").to_string();
    [base, lang]
        .into_iter()
        .filter(|t| !t.is_empty() && t != "C" && t != "POSIX")
        .collect()
}

/// Pick the localized `Name[locale]` for the user's locale, else the plain name.
fn pick_name(
    tags: &[String],
    localized: &[(String, String)],
    plain: Option<String>,
) -> Option<String> {
    for tag in tags {
        if let Some((_, v)) = localized.iter().find(|(t, _)| t.eq_ignore_ascii_case(tag)) {
            return Some(v.clone());
        }
    }
    plain
}

/// freedesktop visibility: `OnlyShowIn`/`NotShowIn` hold `;`-separated desktop
/// names, compared against `XDG_CURRENT_DESKTOP` (`:`-separated, e.g.
/// `sway:wlroots`). An empty current desktop means "do not filter".
/// `.desktop` `TryExec=`: hide the entry when that executable is not runnable.
/// An absolute path is checked directly; a bare name is looked up in `$PATH`.
fn try_exec_ok(cmd: &str) -> bool {
    let runs = |p: &Path| {
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    };
    if cmd.is_empty() {
        return true;
    }
    if cmd.contains('/') {
        return runs(Path::new(cmd));
    }
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .any(|dir| runs(&Path::new(dir).join(cmd)))
}

fn shown_on(only: Option<&str>, not: Option<&str>, current: &str) -> bool {
    let current: Vec<&str> = current.split(':').filter(|s| !s.is_empty()).collect();
    if current.is_empty() {
        return true;
    }
    let lists = |list: &str| {
        list.split(';')
            .any(|d| !d.is_empty() && current.iter().any(|c| c.eq_ignore_ascii_case(d)))
    };
    if let Some(l) = only
        && !lists(l)
    {
        return false;
    }
    if let Some(l) = not
        && lists(l)
    {
        return false;
    }
    true
}

/// Expand/remove Exec field codes per the desktop entry spec: `%f %F %u %U %d %D`
/// (file/URL placeholders, and we pass none), `%i` (icon), `%v`/`%m` (deprecated)
/// are dropped; `%c` expands to the entry name, `%k` to the desktop file path,
/// `%%` is a literal percent. Codes may be attached to words (`app%u`) as well as
/// stand alone, so this scans characters — the old code only stripped *trailing*
/// `%`-words and left a `%u` in the middle of an Exec line, which made the
/// launch fail.
fn clean_exec(exec: &str, name: &str, desktop_file: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('c') => out.push_str(name),
            Some('k') => out.push_str(desktop_file),
            Some(_) => {}
            None => out.push('%'),
        }
    }
    // Dropping a standalone code leaves a double space behind.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Commands found in `$PATH`, deduped by file name and limited to entries that
/// can actually run: directories and non-executable files (both common in PATH
/// dirs) would only become dead menu entries.
pub fn path_commands() -> Vec<String> {
    path_commands_in(&std::env::var("PATH").unwrap_or_default())
}

fn path_commands_in(path: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for dir in path.split(':').filter(|d| !d.is_empty()) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || seen.contains(&name) {
                continue;
            }
            // Follow symlinks (`DirEntry::metadata` does not — PATH entries are
            // usually symlinks, e.g. /usr/bin/sh -> bash) but require a regular
            // executable file; checked before `seen` so a later PATH dir can
            // still supply a runnable entry of the same name.
            let Ok(meta) = std::fs::metadata(e.path()) else {
                continue;
            };
            if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
                continue;
            }
            seen.insert(name.clone());
            out.push(name);
        }
    }
    out
}

/// Merge desktop apps and PATH commands into menu items: apps win on name
/// collision (case-insensitive), result sorted by name.
pub fn merged(apps: Vec<App>, cmds: Vec<String>) -> Vec<crate::items::Item> {
    let mut items: Vec<crate::items::Item> = apps
        .into_iter()
        .map(|a| crate::items::Item {
            lc: a.name.to_lowercase(),
            text: a.name,
            value: a.exec,
            extra: a.keywords,
        })
        .collect();
    let mut seen: HashSet<String> = items.iter().map(|i| i.text.to_lowercase()).collect();
    for cmd in cmds {
        if seen.insert(cmd.to_lowercase()) {
            items.push(crate::items::Item {
                lc: cmd.to_lowercase(),
                text: cmd.clone(),
                value: cmd,
                extra: String::new(),
            });
        }
    }
    items.sort_by(|a, b| a.lc.cmp(&b.lc)); // lc precomputed at parse; no per-comparison lowercasing
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_apps_and_commands_dedups_by_name() {
        let apps = vec![
            App {
                name: "Firefox".into(),
                exec: "firefox %u".into(),
                keywords: "browser".into(),
            },
            App {
                name: "Zathura".into(),
                exec: "zathura %f".into(),
                keywords: String::new(),
            },
        ];
        let cmds = vec![
            "firefox".to_string(),
            "alacritty".to_string(),
            "zathura".to_string(),
        ];
        let items = merged(apps, cmds);
        let names: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        // deduped, sorted, desktop entry wins over the bare command
        assert_eq!(names, vec!["alacritty", "Firefox", "Zathura"]);
    }

    #[test]
    fn path_commands_finds_shell_utilities() {
        let cmds = path_commands();
        assert!(
            cmds.iter().any(|c| c == "sh"),
            "PATH should contain sh: {cmds:?}"
        );
        assert!(cmds.iter().any(|c| c == "ls" || c == "env" || c == "cat"));
    }

    #[test]
    fn path_commands_keeps_only_executable_files() {
        let dir = std::env::temp_dir().join(format!("wlmenu-path-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let runnable = dir.join("runnable");
        std::fs::write(&runnable, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&runnable, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("data.txt"), b"not runnable").unwrap();
        std::fs::create_dir(dir.join("subdir")).unwrap();

        let got = path_commands_in(&dir.display().to_string());
        assert_eq!(
            got,
            vec!["runnable"],
            "only regular executable files are listed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod legacy_tests {
    use super::*;

    #[test]
    fn clean_exec_strips_field_codes() {
        let name = "App";
        let file = "/apps/app.desktop";
        assert_eq!(clean_exec("firefox %u", name, file), "firefox");
        assert_eq!(
            clean_exec("alacritty -e fish %F", name, file),
            "alacritty -e fish"
        );
        assert_eq!(
            clean_exec("code --new-window %F %U", name, file),
            "code --new-window"
        );
        assert_eq!(
            clean_exec("env FOO=1 app --flag", name, file),
            "env FOO=1 app --flag"
        );
        // Field codes in the middle of the line (and attached to a word) must go
        // too: leaving them makes the launch fail with a literal `%u`.
        assert_eq!(clean_exec("app %u --flag", name, file), "app --flag");
        assert_eq!(clean_exec("--open=app%u", name, file), "--open=app");
        assert_eq!(clean_exec("app --name=%c", name, file), "app --name=App");
        assert_eq!(
            clean_exec("app --file=%k", name, file),
            "app --file=/apps/app.desktop"
        );
        // `%%` is a literal percent, not a field code.
        assert_eq!(clean_exec("app 100%%", name, file), "app 100%");
    }

    #[test]
    fn localized_name_wins_over_plain_name() {
        let names = vec![
            ("zh_CN".to_string(), "火狐".to_string()),
            ("de".to_string(), "Feuerfuchs".to_string()),
        ];
        let zh = vec!["zh_CN".to_string(), "zh".to_string()];
        assert_eq!(
            pick_name(&zh, &names, Some("Firefox".into())),
            Some("火狐".into())
        );
        // An unknown language falls back to the plain name.
        let fr = vec!["fr".to_string()];
        assert_eq!(
            pick_name(&fr, &names, Some("Firefox".into())),
            Some("Firefox".into())
        );
        assert_eq!(pick_name(&zh, &[], None), None);
    }

    #[test]
    fn try_exec_hides_entries_whose_binary_is_missing() {
        assert!(try_exec_ok(""), "no TryExec means no constraint");
        assert!(try_exec_ok("sh"), "sh is in PATH");
        assert!(!try_exec_ok("wlmenu-definitely-not-installed-xyz"));
        assert!(!try_exec_ok("/nonexistent/dir/binary"));
    }

    #[test]
    fn desktop_visibility_filters_by_current_desktop() {
        assert!(
            !shown_on(Some("GNOME;"), None, "sway:wlroots"),
            "other-DE entry is hidden"
        );
        assert!(shown_on(Some("sway;"), None, "sway:wlroots"));
        assert!(shown_on(Some("wlroots;"), None, "sway:wlroots"));
        assert!(
            !shown_on(None, Some("sway;"), "sway:wlroots"),
            "NotShowIn wins"
        );
        assert!(shown_on(None, Some("GNOME;"), "sway:wlroots"));
        assert!(shown_on(None, None, "sway"));
        assert!(
            shown_on(Some("GNOME;"), Some("sway;"), ""),
            "no current desktop: no filtering"
        );
    }

    #[test]
    fn parses_desktop_file() {
        let dir = std::env::temp_dir().join("wlmenu-test-apps");
        let _ = std::fs::create_dir_all(&dir);
        let df = dir.join("test-app.desktop");
        std::fs::write(
            &df,
            "[Desktop Entry]\nType=Application\nName=Test 应用\nIcon=org.test.App\n\
             Exec=test-app --flag %U\nNoDisplay=false\n",
        )
        .unwrap();
        let app = parse_desktop(&df).expect("parses");
        assert_eq!(app.name, "Test 应用");
        assert_eq!(app.exec, "test-app --flag");

        std::fs::write(
            &df,
            "[Desktop Entry]\nType=Application\nName=Hide Me\nNoDisplay=true\n",
        )
        .unwrap();
        assert!(parse_desktop(&df).is_none());

        // Terminal apps and entries for another desktop are not listed.
        std::fs::write(
            &df,
            "[Desktop Entry]\nName=Needs TTY\nExec=htop\nTerminal=true\n",
        )
        .unwrap();
        assert!(
            parse_desktop(&df).is_none(),
            "Terminal=true is a dead entry here"
        );
        std::fs::write(
            &df,
            "[Desktop Entry]\nName=GNOME Only\nExec=gnome-thing\nOnlyShowIn=GNOME;\n",
        )
        .unwrap();
        let current = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        assert_eq!(
            parse_desktop(&df).is_some(),
            shown_on(Some("GNOME;"), None, &current)
        );
        std::fs::write(
            &df,
            "[Desktop Entry]\nName=Missing Bin\nExec=ghost\nTryExec=wlmenu-definitely-not-installed-xyz\n",
        )
        .unwrap();
        assert!(parse_desktop(&df).is_none(), "TryExec hid a missing binary");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scans_system_dirs() {
        let apps = load_apps();
        assert!(
            !apps.is_empty(),
            "system /usr/share/applications should yield entries"
        );
        assert!(
            apps.windows(2)
                .all(|w| w[0].name.to_lowercase() <= w[1].name.to_lowercase())
        );
    }
}
