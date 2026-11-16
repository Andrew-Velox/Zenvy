//! Installed applications for the phone's launcher.
//!
//! Applications come only from `.desktop` entries in the standard XDG directories. A phone sends an entry *id*; the
//! command line comes from the entry on this PC and is run as an argument vector, never through a shell, so nothing the
//! phone sends is ever interpreted as a command. Entries that need a terminal, are hidden, or name a missing program
//! are not listed.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_ENTRY_BYTES: u64 = 64 * 1024;
const MAX_APPS: usize = 1500;
const MAX_DEPTH: usize = 3;
const RESCAN_AFTER: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// The desktop file id without `.desktop` (`org.mozilla.firefox`); also what the icon lookup takes.
    pub id: String,
    pub name: String,
    argv: Vec<String>,
}

impl App {
    /// The program name only (never the arguments), for logs.
    pub fn program(&self) -> &str {
        self.argv.first().map(String::as_str).unwrap_or("")
    }
}

/// An id is a file name: letters, digits and `._+-`, no leading dot.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 120
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
}

pub struct Apps {
    dirs: Vec<PathBuf>,
    desktops: Vec<String>,
    path: Vec<PathBuf>,
    cache: Mutex<Option<(Instant, Arc<Vec<App>>)>>,
}

impl Apps {
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut dirs: Vec<PathBuf> = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(".local/share")))
            .into_iter()
            .collect();
        let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
        let sys = if sys.is_empty() {
            "/usr/local/share:/usr/share".to_string()
        } else {
            sys
        };
        dirs.extend(sys.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
        let desktops = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .split(':')
            .filter(|d| !d.is_empty())
            .map(str::to_string)
            .collect();
        let path = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        Self::new(
            dirs.into_iter().map(|d| d.join("applications")).collect(),
            desktops,
            path,
        )
    }

    pub fn new(dirs: Vec<PathBuf>, desktops: Vec<String>, path: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            desktops,
            path,
            cache: Mutex::new(None),
        }
    }

    /// Blocking (reads the file system); the result is reused for a few seconds.
    pub fn list(&self) -> Arc<Vec<App>> {
        let mut c = self.cache.lock().unwrap();
        if let Some((at, apps)) = c.as_ref() {
            if at.elapsed() < RESCAN_AFTER {
                return apps.clone();
            }
        }
        let apps = Arc::new(self.scan());
        *c = Some((Instant::now(), apps.clone()));
        apps
    }

    pub fn find(&self, id: &str) -> Option<App> {
        if !valid_id(id) {
            return None;
        }
        self.list().iter().find(|a| a.id == id).cloned()
    }

    fn scan(&self) -> Vec<App> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for dir in &self.dirs {
            self.walk(dir, dir, 0, &mut seen, &mut out);
        }
        out.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        out
    }

    fn walk(
        &self,
        root: &Path,
        dir: &Path,
        depth: usize,
        seen: &mut HashSet<String>,
        out: &mut Vec<App>,
    ) {
        if depth > MAX_DEPTH || out.len() >= MAX_APPS {
            return;
        }
        let Ok(rd) = fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let path = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                self.walk(root, &path, depth + 1, seen, out);
                continue;
            }
            let Some(file) = path.file_name().and_then(|f| f.to_str()) else {
                continue;
            };
            let Some(stem) = file.strip_suffix(".desktop") else {
                continue;
            };
            // `a/b.desktop` has the id `a-b`, as the specification says
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let mut id = rel.with_extension("").to_string_lossy().replace('/', "-");
            if id.is_empty() {
                id = stem.to_string();
            }
            if !valid_id(&id) || seen.contains(&id) {
                continue;
            }
            // a hidden entry in a user directory also hides the system one of the same id
            seen.insert(id.clone());
            if let Some(app) = self.read_entry(&id, &path) {
                if out.len() < MAX_APPS {
                    out.push(app);
                }
            }
        }
    }

    fn read_entry(&self, id: &str, path: &Path) -> Option<App> {
        let meta = fs::metadata(path).ok()?;
        if !meta.is_file() || meta.len() > MAX_ENTRY_BYTES {
            return None;
        }
        let text = fs::read_to_string(path).ok()?;
        let app = parse_entry(id, &text, &self.desktops)?;
        // TryExec: not installed means not listed
        let try_exec = entry_value(&text, "TryExec");
        if let Some(t) = try_exec {
            if !self.exists(&t) {
                return None;
            }
        }
        Some(app)
    }

    fn exists(&self, prog: &str) -> bool {
        let is_exec = |p: &Path| {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(p)
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        };
        if prog.contains('/') {
            return is_exec(Path::new(prog));
        }
        self.path.iter().any(|d| is_exec(&d.join(prog)))
    }
}

/// The first `key=value` of the `[Desktop Entry]` group.
fn entry_value(text: &str, key: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == key {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

fn truthy(v: Option<String>) -> bool {
    v.as_deref() == Some("true")
}

fn list_has(v: &str, desktops: &[String]) -> bool {
    v.split(';')
        .filter(|s| !s.is_empty())
        .any(|s| desktops.iter().any(|d| d == s))
}

/// Parses one desktop entry. `None` for anything that should not be offered.
pub fn parse_entry(id: &str, text: &str, desktops: &[String]) -> Option<App> {
    if entry_value(text, "Type").as_deref() != Some("Application") {
        return None;
    }
    if truthy(entry_value(text, "NoDisplay"))
        || truthy(entry_value(text, "Hidden"))
        || truthy(entry_value(text, "Terminal"))
    {
        return None;
    }
    if let Some(only) = entry_value(text, "OnlyShowIn") {
        if !list_has(&only, desktops) {
            return None;
        }
    }
    if let Some(not) = entry_value(text, "NotShowIn") {
        if list_has(&not, desktops) {
            return None;
        }
    }
    let name: String = entry_value(text, "Name")?
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    if name.trim().is_empty() {
        return None;
    }
    let argv = parse_exec(&entry_value(text, "Exec")?, &name)?;
    Some(App {
        id: id.to_string(),
        name,
        argv,
    })
}

/// Splits an `Exec` value into an argument vector following the Desktop Entry rules: double quotes group an
/// argument (with `\"`, `` \` ``, `\$` and `\\` escapes), field codes are dropped (no files or URLs are passed),
/// `%c` becomes the name and `%%` a percent sign. `None` if nothing is left or a quote is never closed.
pub fn parse_exec(exec: &str, name: &str) -> Option<Vec<String>> {
    let mut args: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' => quoted = false,
                '\\' => match chars.peek() {
                    Some(&n) if "\"`$\\".contains(n) => {
                        cur.push(n);
                        chars.next();
                    }
                    _ => cur.push('\\'),
                },
                _ => cur.push(c),
            }
        } else if c == '"' {
            quoted = true;
            started = true;
        } else if c.is_whitespace() {
            if started {
                args.push(std::mem::take(&mut cur));
                started = false;
            }
        } else {
            cur.push(c);
            started = true;
        }
    }
    if quoted {
        return None;
    }
    if started {
        args.push(cur);
    }
    let mut out = Vec::new();
    for a in args {
        // a whole argument that is a field code disappears; inside a longer argument the code is removed
        if a.len() == 2 && a.starts_with('%') && !a.starts_with("%%") {
            if a == "%c" {
                out.push(name.to_string());
            }
            continue;
        }
        let mut s = String::new();
        let mut it = a.chars().peekable();
        while let Some(c) = it.next() {
            if c == '%' {
                match it.next() {
                    Some('%') => s.push('%'),
                    Some('c') => s.push_str(name),
                    Some(_) | None => {}
                }
            } else {
                s.push(c);
            }
        }
        out.push(s);
    }
    if out.first().map(|p| p.is_empty()).unwrap_or(true) {
        return None;
    }
    Some(out)
}

/// Starts the application detached (its own process group, no terminal, no output) and reaps it when it ends.
/// `Err` carries a short reason, never the command line.
pub fn launch(app: &App) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let (prog, args) = app.argv.split_first().ok_or("empty command")?;
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_remove("DESKTOP_STARTUP_ID")
        .env_remove("NOTIFY_SOCKET")
        .process_group(0);
    if let Some(home) = std::env::var_os("HOME") {
        cmd.current_dir(home);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start: {}", e.kind()))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn exec_lines_follow_the_desktop_entry_rules() {
        assert_eq!(parse_exec("kitty", "K"), Some(d(&["kitty"])));
        assert_eq!(
            parse_exec("firefox %u", "F"),
            Some(d(&["firefox"])),
            "field codes are dropped"
        );
        assert_eq!(
            parse_exec(r#""/opt/My App/run" --flag "a b" %F"#, "M"),
            Some(d(&["/opt/My App/run", "--flag", "a b"]))
        );
        assert_eq!(
            parse_exec(r#"sh -c "echo \"hi\" \$HOME""#, "S"),
            Some(d(&["sh", "-c", r#"echo "hi" $HOME"#]))
        );
        assert_eq!(
            parse_exec("app --title=%c --path=%f 100%%", "My App"),
            Some(d(&["app", "--title=My App", "--path=", "100%"]))
        );
        assert_eq!(
            parse_exec("env FOO=1 app %U", "A"),
            Some(d(&["env", "FOO=1", "app"]))
        );
        assert_eq!(parse_exec("", "A"), None);
        assert_eq!(parse_exec("%u", "A"), None, "nothing left to run");
        assert_eq!(parse_exec(r#""unclosed"#, "A"), None);
    }

    fn entry(extra: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName=Calc\nExec=calc %U\n{extra}")
    }

    #[test]
    fn only_launchable_graphical_entries_are_listed() {
        let desk = d(&["Hyprland"]);
        assert!(parse_entry("calc", &entry(""), &desk).is_some());
        for bad in [
            "NoDisplay=true\n",
            "Hidden=true\n",
            "Terminal=true\n",
            "OnlyShowIn=GNOME;KDE;\n",
            "NotShowIn=Hyprland;\n",
        ] {
            assert!(parse_entry("calc", &entry(bad), &desk).is_none(), "{bad}");
        }
        assert!(parse_entry("calc", &entry("OnlyShowIn=Hyprland;\n"), &desk).is_some());
        assert!(parse_entry("x", "[Desktop Entry]\nType=Link\nName=L\nExec=x\n", &desk).is_none());
        assert!(parse_entry("x", "[Desktop Entry]\nType=Application\nExec=x\n", &desk).is_none());
        // keys of other groups (actions) are not read
        let t = "[Desktop Entry]\nType=Application\nName=A\nExec=a\n[Desktop Action new]\nName=B\nExec=evil\n";
        let a = parse_entry("a", t, &desk).unwrap();
        assert_eq!((a.name.as_str(), a.program()), ("A", "a"));
    }

    #[test]
    fn scanning_finds_entries_hides_the_missing_and_prefers_the_user_directory() {
        let t = tempfile::tempdir().unwrap();
        let user = t.path().join("user");
        let sys = t.path().join("sys");
        let bin = t.path().join("bin");
        for p in [&user, &sys, &bin, &sys.join("sub")] {
            fs::create_dir_all(p).unwrap();
        }
        let w = |p: PathBuf, s: String| fs::write(p, s).unwrap();
        w(
            user.join("calc.desktop"),
            entry("").replace("Name=Calc", "Name=My Calc"),
        );
        w(sys.join("calc.desktop"), entry(""));
        w(
            sys.join("zed.desktop"),
            "[Desktop Entry]\nType=Application\nName=Zed\nExec=zed\n".into(),
        );
        w(
            sys.join("sub/inner.desktop"),
            "[Desktop Entry]\nType=Application\nName=Inner\nExec=inner\n".into(),
        );
        w(
            sys.join("gone.desktop"),
            entry("TryExec=not-installed-anywhere\n"),
        );
        w(
            sys.join("here.desktop"),
            entry("TryExec=tool\n").replace("Calc", "Here"),
        );
        let tool = bin.join("tool");
        fs::write(&tool, "#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        // an entry hidden in the user directory hides the system one
        w(
            user.join("zed.desktop"),
            "[Desktop Entry]\nType=Application\nName=Zed\nExec=zed\nHidden=true\n".into(),
        );
        fs::write(sys.join("notes.txt"), "x").unwrap();

        let apps = Apps::new(vec![user, sys], d(&[]), vec![bin]);
        let list = apps.list();
        let names: Vec<_> = list.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Here", "Inner", "My Calc"], "{names:?}");
        assert_eq!(apps.find("sub-inner").unwrap().name, "Inner");
        assert!(apps.find("zed").is_none() && apps.find("gone").is_none());
        for bad in ["", "../x", "a/b", ".hidden", "a b", "x;y"] {
            assert!(apps.find(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn random_entries_and_exec_lines_never_panic() {
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let alphabet: Vec<char> = "%\"\\ \t\n=;[]$`'abcFfUuckiI0é日\u{0}\u{202e}"
            .chars()
            .collect();
        for _ in 0..20000 {
            let n = (next() % 80) as usize;
            let s: String = (0..n)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect();
            let _ = parse_exec(&s, "N");
            let entry = format!(
                "[Desktop Entry]\nType=Application\nName={s}\nExec={s}\nOnlyShowIn={s}\n{s}"
            );
            let _ = parse_entry("x", &entry, &d(&["Hyprland"]));
        }
    }

    #[test]
    fn launching_runs_the_program_without_a_shell() {
        let t = tempfile::tempdir().unwrap();
        let marker = t.path().join("started");
        let app = App {
            id: "t".into(),
            name: "T".into(),
            argv: d(&["touch", marker.to_str().unwrap()]),
        };
        launch(&app).unwrap();
        for _ in 0..50 {
            if marker.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(marker.exists());
        let bad = App {
            id: "b".into(),
            name: "B".into(),
            argv: d(&["/nonexistent/program"]),
        };
        assert!(launch(&bad).unwrap_err().starts_with("could not start"));
    }
}
