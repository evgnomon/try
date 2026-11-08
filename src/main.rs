//! try: ephemeral workspace manager for bash.
//!
//! The binary never changes the shell itself: it prints a bash script on
//! stdout, which the function from `try init` evals. The TUI draws on stderr.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SCRIPT_WARNING: &str =
    "# if you can read this, you didn't launch try from its shell function. run: try --help";

// ---------------------------------------------------------------- styles

const RESET: &str = "\x1b[0m";
const RESET_FG: &str = "\x1b[39m";
const RESET_BOLD: &str = "\x1b[22m";
const ACCENT: &str = "\x1b[1;38;5;214m";
const HIGHLIGHT: &str = "\x1b[1;33m";
const MUTED: &str = "\x1b[38;5;245m";
const SELECTED_BG: &str = "\x1b[48;5;238m";
const SELECTED_FG: &str = "\x1b[38;5;255m";
const DANGER_BG: &str = "\x1b[48;5;52m";

fn dim(s: &str) -> String {
    format!("{MUTED}{s}{RESET_FG}")
}
fn bold(s: &str) -> String {
    format!("\x1b[1m{s}{RESET_BOLD}")
}
fn accent(s: &str) -> String {
    format!("{ACCENT}{s}{RESET_FG}{RESET_BOLD}")
}
fn highlight(s: &str) -> String {
    format!("{HIGHLIGHT}{s}{RESET_FG}{RESET_BOLD}")
}

fn char_width(c: char) -> usize {
    match c as u32 {
        0x0300..=0x036F | 0x200B..=0x200D | 0xFE00..=0xFE0F => 0,
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// Width on screen, skipping `ESC [ ... letter` sequences.
fn visible_width(s: &str) -> usize {
    let mut w = 0;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            esc = !c.is_ascii_alphabetic();
        } else if c == '\x1b' {
            esc = true;
        } else {
            w += char_width(c);
        }
    }
    w
}

/// Cuts `s` to `max` columns, ending in "…"; escape sequences are kept.
fn truncate(s: &str, max: usize) -> String {
    if visible_width(s) <= max {
        return s.to_string();
    }
    let target = max.saturating_sub(1);
    let (mut out, mut w, mut esc, mut done) = (String::new(), 0, false, false);
    for c in s.chars() {
        if esc {
            out.push(c);
            esc = !c.is_ascii_alphabetic();
        } else if c == '\x1b' {
            out.push(c);
            esc = true;
        } else if !done {
            let cw = char_width(c);
            if w + cw > target {
                if max > 0 {
                    out.push('…');
                }
                done = true;
            } else {
                out.push(c);
                w += cw;
            }
        }
    }
    out
}

/// Keeps the trailing `max` columns of plain text.
fn truncate_start(s: &str, max: usize) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    let mut w: usize = chars.iter().map(|&c| char_width(c)).sum();
    while w > max && !chars.is_empty() {
        w -= char_width(chars.remove(0));
    }
    chars.into_iter().collect()
}

// ---------------------------------------------------------------- frame

#[derive(Default)]
struct Line {
    text: String,
    center: bool,
    /// Plain text drawn at the right edge, styled with `right_style`.
    right: String,
    right_style: &'static str,
    bg: String,
    /// Column of the input cursor within `text`.
    cursor: Option<usize>,
}

impl Line {
    fn new(text: impl Into<String>) -> Line {
        Line { text: text.into(), ..Default::default() }
    }
    fn centered(text: impl Into<String>) -> Line {
        Line { text: text.into(), center: true, ..Default::default() }
    }
}

#[derive(Default)]
struct Frame {
    header: Vec<Line>,
    body: Vec<Line>,
    footer: Vec<Line>,
}

/// One screen row; returns it and the 1-based cursor column if it has one.
fn render_line(l: &Line, width: usize, newline: bool) -> (String, Option<usize>) {
    let max = width.saturating_sub(1);
    let mut out = String::from("\r\x1b[K");
    out.push_str(&l.bg);
    let text = truncate(&l.text, max);
    let tw = visible_width(&text);
    let start = if l.center { max.saturating_sub(tw) / 2 } else { 0 };
    out.push_str(&" ".repeat(start));
    out.push_str(&text);
    let pos = start + tw;
    let right = if l.right.is_empty() {
        String::new()
    } else {
        truncate_start(&l.right, max.saturating_sub(pos + 1))
    };
    let fill_to = max - visible_width(&right);
    out.push_str(&" ".repeat(fill_to.saturating_sub(pos)));
    if !right.is_empty() {
        out.push_str(l.right_style);
        out.push_str(&right);
        out.push_str(RESET_FG);
    }
    out.push_str(RESET);
    if newline {
        out.push('\n');
    }
    (out, l.cursor.map(|c| start + c + 1))
}

/// The whole frame as one string: header on top, footer pinned to the bottom.
fn render(f: &Frame, (height, width): (usize, usize)) -> String {
    let body_space = height.saturating_sub(f.header.len() + f.footer.len());
    let blank = Line::default();
    let mut rows: Vec<&Line> = f.header.iter().collect();
    rows.extend(f.body.iter().take(body_space));
    for _ in f.body.len().min(body_space)..body_space {
        rows.push(&blank);
    }
    rows.extend(f.footer.iter());

    let mut buf = String::from("\x1b[H");
    let mut cursor = None;
    for (i, l) in rows.iter().enumerate() {
        let (s, col) = render_line(l, width, i + 1 < rows.len());
        buf.push_str(&s);
        if let Some(col) = col {
            cursor = Some((i + 1, col));
        }
    }
    match cursor {
        Some((r, c)) => buf.push_str(&format!("\x1b[{r};{c}H\x1b[?25h")),
        None => buf.push_str("\x1b[?25l"),
    }
    buf.push_str(RESET);
    buf
}

fn show(f: &Frame, size: (usize, usize)) {
    let mut e = io::stderr().lock();
    let _ = e.write_all(render(f, size).as_bytes());
    let _ = e.flush();
}

fn rule(width: usize) -> String {
    dim(&"─".repeat(width.saturating_sub(1)))
}

// ---------------------------------------------------------------- terminal

/// Raw mode and the alternate screen for as long as it lives.
struct Term {
    saved: libc::termios,
}

impl Term {
    fn enter() -> io::Result<Term> {
        // SAFETY: plain termios calls on stdin with a zeroed struct.
        let saved = unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return Err(io::Error::last_os_error());
            }
            let saved = t;
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(0, libc::TCSANOW, &t);
            saved
        };
        eprint!("\x1b[?1049h");
        Ok(Term { saved })
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        eprint!("{RESET}\x1b[?25h\x1b[?1049l");
        // SAFETY: restores the attributes saved in enter.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.saved);
            libc::tcflush(0, libc::TCIFLUSH);
        }
    }
}

fn term_size() -> (usize, usize) {
    // SAFETY: TIOCGWINSZ fills a winsize struct.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
            return (ws.ws_row as usize, ws.ws_col as usize);
        }
    }
    (24, 80)
}

fn is_tty(fd: i32) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

fn poll_stdin(ms: i32) -> bool {
    let mut p = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd.
    unsafe { libc::poll(&mut p, 1, ms) > 0 }
}

fn read_byte() -> Option<u8> {
    let mut b = 0u8;
    // SAFETY: reads one byte into b.
    let n = unsafe { libc::read(0, (&mut b as *mut u8).cast(), 1) };
    (n == 1).then_some(b)
}

fn read_more(bytes: &mut Vec<u8>) -> Option<u8> {
    let b = if poll_stdin(10) { read_byte()? } else { return None };
    bytes.push(b);
    Some(b)
}

/// One key: a byte, a UTF-8 character or an escape sequence. None when
/// nothing came within `ms`, so the caller can check for a resize.
fn read_key(ms: i32) -> Option<String> {
    if !poll_stdin(ms) {
        return None;
    }
    let Some(b) = read_byte() else {
        return Some("\x03".into()); // end of input: cancel
    };
    let mut bytes = vec![b];
    if b == 0x1b {
        match read_more(&mut bytes) {
            Some(b'[') => {
                // CSI: read to the final byte so mouse and unknown keys never
                // leak into the search field.
                while let Some(c) = read_more(&mut bytes) {
                    if (0x40..=0x7e).contains(&c) {
                        break;
                    }
                }
                if bytes == b"\x1b[M" {
                    for _ in 0..3 {
                        read_more(&mut bytes);
                    }
                }
            }
            Some(b'O') => {
                read_more(&mut bytes);
            }
            _ => {}
        }
    } else if b >= 0xc0 {
        let extra = if b >= 0xf0 { 3 } else if b >= 0xe0 { 2 } else { 1 };
        for _ in 0..extra {
            read_more(&mut bytes);
        }
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

// ---------------------------------------------------------------- input field

struct Input {
    text: Vec<char>,
    cursor: usize,
}

impl Input {
    fn new(s: &str) -> Input {
        let text: Vec<char> = s.chars().collect();
        Input { cursor: text.len(), text }
    }

    fn text(&self) -> String {
        self.text.iter().collect()
    }

    /// Emacs-style editing; false when the key is not for the field.
    fn handle(&mut self, key: &str) -> bool {
        let len = self.text.len();
        match key {
            "\x7f" | "\x08" => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.text.remove(self.cursor);
                }
            }
            "\x1b[3~" => {
                if self.cursor < len {
                    self.text.remove(self.cursor);
                }
            }
            "\x01" | "\x1b[H" | "\x1b[1~" | "\x1b[7~" | "\x1bOH" => self.cursor = 0,
            "\x05" | "\x1b[F" | "\x1b[4~" | "\x1b[8~" | "\x1bOF" => self.cursor = len,
            "\x02" => self.cursor = self.cursor.saturating_sub(1),
            "\x06" => self.cursor = (self.cursor + 1).min(len),
            "\x0b" => self.text.truncate(self.cursor),
            "\x15" => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            "\x17" => {
                let mut p = self.cursor;
                while p > 0 && !self.text[p - 1].is_ascii_alphanumeric() {
                    p -= 1;
                }
                while p > 0 && self.text[p - 1].is_ascii_alphanumeric() {
                    p -= 1;
                }
                self.text.drain(p..self.cursor);
                self.cursor = p;
            }
            k if is_arrow(k, 'D') => self.cursor = self.cursor.saturating_sub(1),
            k if is_arrow(k, 'C') => self.cursor = (self.cursor + 1).min(len),
            k => {
                let mut cs = k.chars();
                match (cs.next(), cs.next()) {
                    (Some(c), None) if c as u32 >= 32 && c != '\x7f' => {
                        self.text.insert(self.cursor, c);
                        self.cursor += 1;
                    }
                    _ => return false,
                }
            }
        }
        true
    }

    /// The text with a reverse-video cursor, and the cursor's column.
    fn view(&self) -> (String, usize) {
        let before: String = self.text[..self.cursor].iter().collect();
        let cur = self.text.get(self.cursor).copied().unwrap_or(' ');
        let after: String = self.text.iter().skip(self.cursor + 1).collect();
        (format!("{before}\x1b[7m{cur}\x1b[27m{after}"), visible_width(&before))
    }
}

fn is_arrow(k: &str, last: char) -> bool {
    k == format!("\x1b[{last}")
        || k == format!("\x1bO{last}")
        || (k.starts_with("\x1b[") && k.ends_with(last) && k.len() > 3)
}

// ---------------------------------------------------------------- fuzzy

fn lower_char(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Subsequence match of `query` in `text` (both lowercased). Returns the
/// score and the matched character positions, or None.
fn fuzzy(text: &[char], query: &[char], base_score: f64) -> Option<(f64, Vec<usize>)> {
    let mut score = base_score;
    let mut positions = Vec::with_capacity(query.len());
    if query.is_empty() {
        return Some((score, positions));
    }
    let mut last: Option<usize> = None;
    let mut from = 0;
    for &qc in query {
        let found = (from..text.len()).find(|&i| text[i] == qc)?;
        positions.push(found);
        score += 1.0;
        if found == 0 || !(text[found - 1].is_ascii_lowercase() || text[found - 1].is_ascii_digit()) {
            score += 1.0;
        }
        if let Some(l) = last {
            let gap = found - l - 1;
            score += 2.0 / ((gap + 1) as f64).sqrt();
        }
        last = Some(found);
        from = found + 1;
    }
    score *= query.len() as f64 / (last.unwrap_or(0) + 1) as f64;
    score *= 10.0 / (text.len() as f64 + 10.0);
    Some((score, positions))
}

// ---------------------------------------------------------------- helpers

/// Bash single-quoting.
fn q(s: impl AsRef<str>) -> String {
    format!("'{}'", s.as_ref().replace('\'', r#"'"'"'"#))
}

fn qp(p: &Path) -> String {
    q(p.to_string_lossy())
}

/// Each run of whitespace becomes one "-".
fn dashify(s: &str) -> String {
    let mut out = String::new();
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push('-');
            }
            in_ws = true;
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

fn is_date_prefixed(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 11
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'-'
        && [0, 1, 2, 3, 5, 6, 8, 9].iter().all(|&i| b[i].is_ascii_digit())
}

fn strip_date(name: &str) -> &str {
    if is_date_prefixed(name) { &name[11..] } else { name }
}

fn today() -> String {
    // SAFETY: localtime_r fills tm from the current time.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
    }
}

fn cwd() -> PathBuf {
    let real = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    if let Ok(pwd) = env::var("PWD") {
        let pwd = PathBuf::from(pwd);
        if pwd.is_absolute() && fs::canonicalize(&pwd).ok() == fs::canonicalize(&real).ok() {
            return pwd;
        }
    }
    real
}

/// Like Ruby's File.expand_path: `~`, relative paths, `.` and `..`.
fn expand(p: &str) -> PathBuf {
    let home = env::var("HOME").unwrap_or_default();
    let path = if p == "~" {
        PathBuf::from(&home)
    } else if let Some(rest) = p.strip_prefix("~/") {
        Path::new(&home).join(rest)
    } else {
        PathBuf::from(p)
    };
    let abs = if path.is_absolute() { path } else { cwd().join(path) };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

fn username() -> String {
    for var in ["USER", "LOGNAME"] {
        if let Ok(u) = env::var(var)
            && !u.is_empty()
        {
            return u;
        }
    }
    // SAFETY: getpwuid returns a pointer to static storage or null.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() {
            return std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned();
        }
    }
    expand("~").file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `$TRY_PATH`, or ~/src/github.com/<user>/tries.
fn tries_path() -> PathBuf {
    match env::var("TRY_PATH") {
        Ok(p) if !p.is_empty() => expand(&p),
        _ => expand(&format!("~/src/github.com/{}/tries", username())),
    }
}

fn rel_time(t: SystemTime) -> String {
    let secs = SystemTime::now().duration_since(t).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let (m, h, d) = (secs / 60.0, secs / 3600.0, secs / 86400.0);
    if secs < 60.0 {
        "just now".into()
    } else if m < 60.0 {
        format!("{}m ago", m as u64)
    } else if h < 24.0 {
        format!("{}h ago", h as u64)
    } else if d < 7.0 {
        format!("{}d ago", d as u64)
    } else {
        format!("{}w ago", (d / 7.0) as u64)
    }
}

/// `base` itself if `DATE-base` is free; else bump a trailing number
/// (`v2` → `v3`), or append `-2`, `-3`, ...
fn versioned_name(dir: &Path, date: &str, base: &str) -> String {
    let taken = |b: &str| dir.join(format!("{date}-{b}")).is_dir();
    if !taken(base) {
        return base.to_string();
    }
    let stem = base.trim_end_matches(|c: char| c.is_ascii_digit());
    if stem.len() < base.len()
        && let Ok(n) = base[stem.len()..].parse::<u64>()
    {
        let mut i = n + 1;
        loop {
            let cand = format!("{stem}{i}");
            if !taken(&cand) {
                return cand;
            }
            i += 1;
        }
    }
    let mut i = 2;
    loop {
        let cand = format!("{base}-{i}");
        if !taken(&cand) {
            return cand;
        }
        i += 1;
    }
}

// ---------------------------------------------------------------- git URIs

fn is_git_uri(arg: &str) -> bool {
    arg.starts_with("http://")
        || arg.starts_with("https://")
        || arg.starts_with("git@")
        || arg.contains("github.com")
        || arg.contains("gitlab.com")
        || arg.ends_with(".git")
}

fn basename(p: &str) -> &str {
    p.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

fn strip_scheme(uri: &str) -> Option<&str> {
    uri.strip_prefix("https://").or_else(|| uri.strip_prefix("http://"))
}

/// (user, repo) from https, ssh:// and scp-style URIs.
fn parse_git_uri(uri: &str) -> Option<(String, String)> {
    let uri = uri.strip_suffix(".git").unwrap_or(uri);
    let ok = |s: &str| !s.is_empty();
    let (user, repo) = if let Some(rest) = strip_scheme(uri) {
        let mut parts = rest.split('/');
        let (host, user, repo) = (parts.next()?, parts.next()?, parts.next()?);
        if !ok(host) {
            return None;
        }
        (user, repo)
    } else if let Some(rest) = uri.strip_prefix("ssh://") {
        let (login, rest) = rest.split_once('@')?;
        let (host, rest) = rest.split_once('/')?;
        let (user, path) = rest.split_once('/')?;
        if !ok(login) || login.contains('/') || !ok(host) {
            return None;
        }
        (user, basename(path))
    } else {
        let (left, path) = uri.split_once(':')?;
        let (login, host) = left.split_once('@')?;
        if !ok(login) || login.contains('/') || !ok(host) {
            return None;
        }
        match path.split_once('/') {
            Some((user, rest)) if login == "git" && ok(rest) => (user, basename(rest)),
            _ => (login, basename(path)),
        }
    };
    (ok(user) && ok(repo)).then(|| (user.to_string(), repo.to_string()))
}

struct Pr {
    user: String,
    repo: String,
    id: String,
}

impl Pr {
    fn git_uri(&self) -> String {
        format!("https://github.com/{}/{}.git", self.user, self.repo)
    }
}

/// https://github.com/USER/REPO/pull/N
fn github_pr(uri: &str) -> Option<Pr> {
    let rest = strip_scheme(uri)?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest.strip_prefix("github.com/")?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let p: Vec<&str> = rest.split('/').collect();
    let valid = p.len() == 4
        && !p[0].is_empty()
        && !p[1].is_empty()
        && p[2] == "pull"
        && !p[3].is_empty()
        && p[3].bytes().all(|b| b.is_ascii_digit());
    valid.then(|| Pr {
        user: p[0].into(),
        repo: p[1].strip_suffix(".git").unwrap_or(p[1]).into(),
        id: p[3].into(),
    })
}

// ---------------------------------------------------------------- scripts

fn emit(cmds: &[String]) -> String {
    format!("{SCRIPT_WARNING}\n{}\n", cmds.join(" && \\\n  "))
}

fn script_cd(path: &Path) -> Vec<String> {
    vec![format!("touch {}", qp(path)), format!("echo {}", qp(path)), format!("cd {}", qp(path))]
}

fn script_mkdir_cd(path: &Path) -> Vec<String> {
    let mut v = vec![format!("mkdir -p {}", qp(path))];
    v.extend(script_cd(path));
    v
}

fn script_clone(path: &Path, uri: &str) -> Vec<String> {
    let mut v = vec![
        format!("mkdir -p {}", qp(path)),
        format!("echo {}", q(format!("Using git clone to create this trial from {uri}."))),
        format!("git clone {} {}", q(uri), qp(path)),
    ];
    v.extend(script_cd(path));
    v
}

fn script_clone_pr(path: &Path, pr: &Pr) -> Vec<String> {
    let uri = pr.git_uri();
    let mut v = vec![
        format!("mkdir -p {}", qp(path)),
        format!("echo {}", q(format!("Using git clone to create this trial from {uri} PR #{}.", pr.id))),
        format!("git clone {} {}", q(&uri), qp(path)),
        format!("git -C {} fetch origin {}", qp(path), q(format!("pull/{}/head", pr.id))),
        format!("git -C {} checkout --detach FETCH_HEAD", qp(path)),
    ];
    v.extend(script_cd(path));
    v
}

fn script_worktree(path: &Path, repo: &Path) -> Vec<String> {
    let msg = format!("Using git worktree to create this trial from {}.", repo.display());
    let mut v = vec![
        format!("mkdir -p {}", qp(path)),
        format!("echo {}", q(msg)),
        format!("{{ git -C {} worktree add --detach {} >/dev/null 2>&1 || true; }}", qp(repo), qp(path)),
    ];
    v.extend(script_cd(path));
    v
}

enum Outcome {
    Cd(PathBuf),
    Mkdir(PathBuf),
    Rename { old: String, new: String },
    Graduate { source: PathBuf, dest: PathBuf, name: String },
    Delete { base: PathBuf, names: Vec<String> },
}

fn outcome_script(o: Outcome, base: &Path) -> Vec<String> {
    match o {
        Outcome::Cd(p) => script_cd(&p),
        Outcome::Mkdir(p) => script_mkdir_cd(&p),
        Outcome::Rename { old, new } => {
            let new_path = base.join(&new);
            vec![
                format!("cd {}", qp(base)),
                format!("mv -- {} {}", q(&old), q(&new)),
                format!("echo {}", qp(&new_path)),
                format!("cd {}", qp(&new_path)),
            ]
        }
        Outcome::Graduate { source, dest, name } => {
            let mut v = vec![if source.join(".git").is_file() {
                format!("git worktree move {} {}", qp(&source), qp(&dest))
            } else {
                format!("mv -- {} {}", qp(&source), qp(&dest))
            }];
            v.push(format!("ln -s {} {}", qp(&dest), qp(&base.join(&name))));
            v.push(format!("echo {}", q(format!("Graduated: {name} → {}", dest.display()))));
            v.extend(script_cd(&dest));
            v
        }
        Outcome::Delete { base, names } => {
            let mut v = vec![format!("cd {}", qp(&base))];
            v.extend(names.iter().map(|n| format!("rm -rf -- {}", q(n))));
            v.push(format!("cd {} 2>/dev/null || cd {}", qp(&cwd()), qp(&base)));
            v
        }
    }
}

// ---------------------------------------------------------------- selector

struct Try {
    name: String,
    lower: Vec<char>,
    path: PathBuf,
    symlink: bool,
    mtime: SystemTime,
    base_score: f64,
}

struct Match {
    idx: usize,
    score: f64,
    positions: Vec<usize>,
}

fn load_tries(base: &Path) -> Vec<Try> {
    let now = SystemTime::now();
    let Ok(rd) = fs::read_dir(base) else { return Vec::new() };
    let mut tries = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        let Ok(meta) = fs::metadata(&path) else { continue };
        if !meta.is_dir() {
            continue;
        }
        let mtime = meta.modified().unwrap_or(now);
        let hours = now.duration_since(mtime).map(|d| d.as_secs_f64()).unwrap_or(0.0) / 3600.0;
        let mut base_score = 3.0 / (hours + 1.0).sqrt();
        if is_date_prefixed(&name) {
            base_score += 2.0;
        }
        let symlink = e.file_type().map(|t| t.is_symlink()).unwrap_or(false);
        let path = if symlink { fs::canonicalize(&path).unwrap_or(path) } else { path };
        tries.push(Try {
            lower: name.chars().map(lower_char).collect(),
            name,
            path,
            symlink,
            mtime,
            base_score,
        });
    }
    tries
}

/// The name with the date dimmed and matched characters highlighted.
fn format_name(name: &str, positions: &[usize], selected: bool) -> String {
    let chars: Vec<char> = name.chars().collect();
    let after = if selected { SELECTED_FG } else { "" };
    let mut out = String::new();
    let mut i = 0;
    if is_date_prefixed(name) {
        let date: String = chars[..10].iter().collect();
        out += &if selected { date } else { dim(&date) };
        out += &if positions.contains(&10) {
            highlight("-") + after
        } else if selected {
            "-".into()
        } else {
            dim("-")
        };
        i = 11;
    }
    while i < chars.len() {
        if positions.contains(&i) {
            let start = i;
            while i < chars.len() && positions.contains(&i) {
                i += 1;
            }
            out += &highlight(&chars[start..i].iter().collect::<String>());
            out += after;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

struct Selector {
    base: PathBuf,
    search: Input,
    cursor: usize,
    scroll: usize,
    tries: Vec<Try>,
    results: Vec<Match>,
    cache_key: Option<(String, usize)>,
    marked: Vec<PathBuf>,
    status: Option<String>,
}

impl Selector {
    fn new(base: PathBuf, query: &str) -> Selector {
        Selector {
            tries: load_tries(&base),
            base,
            search: Input::new(&dashify(query)),
            cursor: 0,
            scroll: 0,
            results: Vec::new(),
            cache_key: None,
            marked: Vec::new(),
            status: None,
        }
    }

    fn refresh(&mut self, height: usize) {
        let key = (self.search.text(), height);
        if self.cache_key.as_ref() == Some(&key) {
            return;
        }
        let query: Vec<char> = key.0.chars().map(lower_char).collect();
        let mut res: Vec<Match> = self
            .tries
            .iter()
            .enumerate()
            .filter_map(|(idx, t)| {
                fuzzy(&t.lower, &query, t.base_score).map(|(score, positions)| Match { idx, score, positions })
            })
            .collect();
        res.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        res.truncate(height.saturating_sub(6).max(3));
        self.results = res;
        self.cache_key = Some(key);
    }

    fn selected_try(&self) -> Option<&Try> {
        self.results.get(self.cursor).map(|m| &self.tries[m.idx])
    }

    fn run(&mut self) -> Option<Outcome> {
        let _term = Term::enter().ok()?;
        let mut last_size = (0, 0);
        let mut dirty = true;
        loop {
            let size = term_size();
            self.refresh(size.0);
            let n = self.results.len();
            let create = !self.search.text.is_empty();
            let total = n + create as usize;
            self.cursor = self.cursor.min(total.saturating_sub(1));
            if dirty || size != last_size {
                self.draw(size);
                dirty = false;
                last_size = size;
            }
            let Some(key) = read_key(100) else { continue };
            dirty = true;

            let before = self.search.text.clone();
            if self.search.handle(&key) {
                if self.search.text != before {
                    self.cursor = 0;
                }
                continue;
            }
            match key.as_str() {
                "\r" | "\n" => {
                    if !self.marked.is_empty() {
                        if let Some(o) = self.confirm_delete() {
                            return Some(o);
                        }
                    } else if let Some(t) = self.selected_try() {
                        return Some(Outcome::Cd(t.path.clone()));
                    } else if create {
                        let name = dashify(&format!("{}-{}", today(), self.search.text()));
                        return Some(Outcome::Mkdir(self.base.join(name)));
                    }
                }
                "\x1b[A" | "\x1bOA" | "\x10" => self.cursor = self.cursor.saturating_sub(1),
                "\x1b[B" | "\x1bOB" | "\x0e" => self.cursor = (self.cursor + 1).min(total.saturating_sub(1)),
                "\x04" => {
                    if let Some(path) = self.selected_try().map(|t| t.path.clone()) {
                        if let Some(i) = self.marked.iter().position(|p| *p == path) {
                            self.marked.remove(i);
                        } else {
                            self.marked.push(path);
                        }
                    }
                }
                "\x12" => {
                    if self.cursor < n {
                        self.marked.clear();
                        if let Some(o) = self.rename() {
                            return Some(o);
                        }
                    }
                }
                "\x07" => {
                    if self.cursor < n {
                        self.marked.clear();
                        if let Some(o) = self.graduate() {
                            return Some(o);
                        }
                    }
                }
                "\x03" | "\x1b" => {
                    if self.marked.is_empty() {
                        return None;
                    }
                    self.marked.clear();
                }
                _ => {}
            }
        }
    }

    fn draw(&mut self, (height, width): (usize, usize)) {
        let mut f = Frame::default();
        f.header.push(Line::new(accent("Try Directory Selection")));
        f.header.push(Line::new(rule(width)));
        let (view, off) = self.search.view();
        let prefix = "Search: ";
        f.header.push(Line { text: dim(prefix) + &view, cursor: Some(prefix.len() + off), ..Default::default() });
        f.header.push(Line::new(rule(width)));

        f.footer.push(Line::new(rule(width)));
        if let Some(s) = self.status.take() {
            f.footer.push(Line::new(bold(&s)));
        } else if !self.marked.is_empty() {
            f.footer.push(Line {
                text: bold(" DELETE MODE ")
                    + &format!(" {} marked  |  Ctrl-D: Toggle  Enter: Confirm  Esc: Cancel", self.marked.len()),
                bg: DANGER_BG.into(),
                ..Default::default()
            });
        } else {
            f.footer.push(Line::centered(dim(
                "↑/↓: Navigate  Enter: Select  ^R: Rename  ^G: Graduate  ^D: Delete  Esc: Cancel",
            )));
        }

        let max_visible = height.saturating_sub(f.header.len() + f.footer.len()).max(3);
        let n = self.results.len();
        let create = !self.search.text.is_empty();
        let total = n + create as usize;
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + max_visible {
            self.scroll = self.cursor + 1 - max_visible;
        }
        let end = (self.scroll + max_visible).min(total);
        for idx in self.scroll..end {
            let selected = idx == self.cursor;
            if idx < n {
                f.body.push(self.entry_line(&self.results[idx], selected));
            } else {
                if n > 0 {
                    f.body.push(Line::default());
                }
                f.body.push(self.create_line(selected));
            }
        }
        show(&f, (height, width));
    }

    fn entry_line(&self, m: &Match, selected: bool) -> Line {
        let t = &self.tries[m.idx];
        let marked = self.marked.contains(&t.path);
        let bg = match (marked, selected) {
            (true, true) => format!("{DANGER_BG}{SELECTED_FG}"),
            (true, false) => DANGER_BG.into(),
            (false, true) => format!("{SELECTED_BG}{SELECTED_FG}"),
            (false, false) => String::new(),
        };
        let mut text = if selected { highlight("→ ") + SELECTED_FG } else { "  ".into() };
        text += &format_name(&t.name, &m.positions, selected);
        if t.symlink {
            text += &dim("@");
            if selected {
                text += SELECTED_FG;
            }
        }
        Line {
            text,
            bg,
            right: format!("{}, {:.1}", rel_time(t.mtime), m.score),
            right_style: if selected { "" } else { MUTED },
            ..Default::default()
        }
    }

    fn create_line(&self, selected: bool) -> Line {
        let mut text = if selected { highlight("→ ") + SELECTED_FG } else { "  ".into() };
        text += &format!("+ Create new: {}-{}", today(), self.search.text());
        let bg = if selected { format!("{SELECTED_BG}{SELECTED_FG}") } else { String::new() };
        Line { text, bg, ..Default::default() }
    }

    /// A screen with one input line. Enter runs `validate`: Ok ends the
    /// dialog with its value, Err shows the message. Esc gives None.
    fn dialog<T>(
        &self,
        title: &str,
        pre: Vec<Line>,
        label: &str,
        initial: &str,
        post: Vec<Line>,
        mut validate: impl FnMut(&str) -> Result<T, String>,
    ) -> Option<T> {
        let mut input = Input::new(initial);
        let mut error: Option<String> = None;
        let mut dirty = true;
        let mut last_size = (0, 0);
        loop {
            let size = term_size();
            if dirty || size != last_size {
                let mut f = Frame::default();
                f.header.push(Line::centered(accent(title)));
                f.header.push(Line::new(rule(size.1)));
                for l in &pre {
                    f.body.push(Line { text: l.text.clone(), center: l.center, bg: l.bg.clone(), ..Default::default() });
                }
                let (view, off) = input.view();
                f.body.push(Line {
                    text: dim(label) + &view,
                    center: true,
                    cursor: Some(visible_width(label) + off),
                    ..Default::default()
                });
                for l in &post {
                    f.body.push(Line { text: l.text.clone(), center: l.center, ..Default::default() });
                }
                if let Some(e) = &error {
                    f.body.push(Line::default());
                    f.body.push(Line::centered(bold(e)));
                }
                f.footer.push(Line::new(rule(size.1)));
                f.footer.push(Line::centered(dim("Enter: Confirm  Esc: Cancel")));
                show(&f, size);
                dirty = false;
                last_size = size;
            }
            let Some(key) = read_key(100) else { continue };
            dirty = true;
            let before = input.text.clone();
            if input.handle(&key) {
                if input.text != before {
                    error = None;
                }
                continue;
            }
            match key.as_str() {
                "\r" | "\n" => match validate(&input.text()) {
                    Ok(v) => return Some(v),
                    Err(e) => error = Some(e),
                },
                "\x1b" | "\x03" => return None,
                _ => {}
            }
        }
    }

    fn rename(&self) -> Option<Outcome> {
        let name = self.selected_try()?.name.clone();
        let pre = vec![Line::new(name.clone()), Line::default(), Line::default()];
        let new = self.dialog("Rename directory", pre, "New name: ", &name, vec![], |s| {
            let new = dashify(s.trim());
            if new.is_empty() {
                Err("Name cannot be empty".into())
            } else if new.contains('/') {
                Err("Name cannot contain /".into())
            } else if new == name {
                Ok(None)
            } else if exists(&self.base.join(&new)) {
                Err(format!("Directory exists: {new}"))
            } else {
                Ok(Some(new))
            }
        })??;
        Some(Outcome::Rename { old: name, new })
    }

    fn graduate(&self) -> Option<Outcome> {
        let t = self.selected_try()?;
        let projects = env::var("TRY_PROJECTS").ok().filter(|s| !s.is_empty());
        let hint = if projects.is_some() { "$TRY_PROJECTS" } else { "parent of $TRY_PATH" };
        let dir = match &projects {
            Some(p) => expand(p),
            None => self.base.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/")),
        };
        let pre = vec![
            Line::new(t.name.clone()),
            Line::default(),
            Line::centered(dim(&format!("Destination ({hint}: {})", dir.display()))),
        ];
        let post = vec![Line::default(), Line::centered(dim("A symlink will be left in the tries directory"))];
        let initial = dir.join(strip_date(&t.name));
        let dest = self.dialog("Graduate try to project", pre, "Move to: ", &initial.to_string_lossy(), post, |s| {
            let s = s.trim();
            if s.is_empty() {
                return Err("Destination cannot be empty".into());
            }
            let dest = expand(s);
            if exists(&dest) {
                return Err(format!("Destination already exists: {}", dest.display()));
            }
            match dest.parent() {
                Some(p) if p.is_dir() => Ok(dest),
                p => Err(format!("Parent directory does not exist: {}", p.unwrap_or(&dest).display())),
            }
        })?;
        Some(Outcome::Graduate { source: t.path.clone(), dest, name: t.name.clone() })
    }

    fn confirm_delete(&mut self) -> Option<Outcome> {
        let items: Vec<(PathBuf, String)> = self
            .results
            .iter()
            .map(|m| &self.tries[m.idx])
            .filter(|t| self.marked.contains(&t.path))
            .map(|t| (t.path.clone(), t.name.clone()))
            .collect();
        if items.is_empty() {
            return None;
        }
        let n = items.len();
        let title = format!("Delete {n} {}?", if n == 1 { "directory" } else { "directories" });
        let mut pre: Vec<Line> =
            items.iter().map(|(_, name)| Line { text: name.clone(), bg: DANGER_BG.into(), ..Default::default() }).collect();
        pre.push(Line::default());
        pre.push(Line::default());
        let answer = self.dialog(&title, pre, "Type YES to confirm: ", "", vec![], |s| Ok(s.to_string()));
        if answer.as_deref() != Some("YES") {
            self.status = Some("Delete cancelled".into());
            self.marked.clear();
            return None;
        }
        match self.validate_delete(&items) {
            Ok(o) => Some(o),
            Err(e) => {
                self.status = Some(format!("Error: {e}"));
                None
            }
        }
    }

    /// Every target must resolve to somewhere inside the tries directory.
    fn validate_delete(&self, items: &[(PathBuf, String)]) -> Result<Outcome, String> {
        let base = fs::canonicalize(&self.base).map_err(|e| e.to_string())?;
        let mut names = Vec::new();
        for (path, name) in items {
            let real = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
            if real == base || !real.starts_with(&base) {
                return Err(format!("Safety check failed: {} is not inside {}", real.display(), base.display()));
            }
            names.push(name.clone());
        }
        Ok(Outcome::Delete { base, names })
    }
}

// ---------------------------------------------------------------- commands

fn help() -> String {
    format!(
        "try v{VERSION} - ephemeral workspace manager

Add to ~/.bashrc:
  eval \"$(try init)\"

Usage:
  try [query]            Interactive selector, optionally pre-filtered
  try <git-url> [name]   Clone into a dated directory (GitHub PR URLs too)
  try . <name>           New dated directory; a detached worktree in a git repo
  try ./path [name]      The same, from another directory
  try init [path]        Print the bash function
  try --help | --version

Environment:
  TRY_PATH       Tries directory (default: ~/src/github.com/$USER/tries)
  TRY_PROJECTS   Graduate destination (default: parent of TRY_PATH,
                 i.e. ~/src/github.com/$USER)

Keys:
  ↑/↓, Ctrl-P/N         Navigate
  Enter                 Select, or create new
  Ctrl-R                Rename
  Ctrl-G                Graduate (move to projects, leave a symlink)
  Ctrl-D                Mark for deletion
  Esc, Ctrl-C           Cancel
  Ctrl-A/E/B/F/K/U/W    Edit the search line
"
    )
}

fn init_function(exe: &Path, path: Option<&str>) -> String {
    let try_path = match path {
        Some(p) => qp(&expand(p)),
        None => format!("${{TRY_PATH:-{}}}", qp(&tries_path())),
    };
    format!(
        "try() {{
  local out
  out=$(TRY_PATH={try_path} {} \"$@\" 2>/dev/tty) || return
  eval \"$out\"
}}
",
        qp(exe)
    )
}

/// The script for everything but `init`: Ok(None) means cancelled.
fn command(args: &[String], base: &Path) -> Result<Option<Vec<String>>, String> {
    // try . <name> | try ./path [name]
    if let Some(first) = args.first().filter(|a| a.starts_with('.')) {
        let custom = args[1..].join(" ");
        if first == "." && custom.trim().is_empty() {
            return Err("'try .' requires a name argument\nUsage: try . <name>".into());
        }
        let repo = expand(first);
        let name = if custom.trim().is_empty() {
            repo.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            dashify(custom.trim())
        };
        let date = today();
        let path = base.join(format!("{date}-{}", versioned_name(base, &date, &name)));
        return Ok(Some(if repo.join(".git").exists() { script_worktree(&path, &repo) } else { script_mkdir_cd(&path) }));
    }

    let query = args.join(" ");
    let mut words = query.split_whitespace();
    if let Some(uri) = words.next().filter(|w| is_git_uri(w)) {
        let custom = dashify(&words.collect::<Vec<_>>().join(" "));
        let pr = github_pr(uri);
        let name = if !custom.is_empty() {
            custom
        } else {
            let (user, repo) = match &pr {
                Some(pr) => (pr.user.clone(), pr.repo.clone()),
                None => parse_git_uri(uri).ok_or_else(|| format!("Unable to parse git URI: {uri}"))?,
            };
            format!("{}-{user}-{repo}", today())
        };
        let path = base.join(name);
        return Ok(Some(match pr {
            Some(pr) => script_clone_pr(&path, &pr),
            None => script_clone(&path, uri),
        }));
    }

    if !is_tty(0) || !is_tty(2) {
        return Err("try requires an interactive terminal".into());
    }
    fs::create_dir_all(base).map_err(|e| format!("{}: {e}", base.display()))?;
    let mut selector = Selector::new(base.to_path_buf(), &query);
    Ok(selector.run().map(|o| outcome_script(o, base)))
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        eprint!("{}", help());
        return;
    }
    if args.iter().any(|a| a == "-v" || a == "--version") {
        eprintln!("try {VERSION}");
        return;
    }
    if args.first().map(String::as_str) == Some("init") {
        let exe = env::current_exe().and_then(fs::canonicalize).unwrap_or_else(|_| PathBuf::from("try"));
        print!("{}", init_function(&exe, args.get(1).map(String::as_str)));
        return;
    }
    match command(&args, &tries_path()) {
        Ok(Some(cmds)) => print!("{}", emit(&cmds)),
        Ok(None) => std::process::exit(1),
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().map(lower_char).collect()
    }

    fn tmp(name: &str) -> PathBuf {
        let d = env::temp_dir().join(format!("try-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn fuzzy_scores() {
        assert_eq!(fuzzy(&chars("abc"), &[], 1.5).unwrap().0, 1.5);
        assert!(fuzzy(&chars("abc"), &chars("abd"), 0.0).is_none());
        let (_, pos) = fuzzy(&chars("2024-01-15-Project"), &chars("proj"), 0.0).unwrap();
        assert_eq!(pos, vec![11, 12, 13, 14]);
        let tight = fuzzy(&chars("project"), &chars("pro"), 0.0).unwrap().0;
        let loose = fuzzy(&chars("p-r-o-ject"), &chars("pro"), 0.0).unwrap().0;
        assert!(tight > loose);
        let short = fuzzy(&chars("api"), &chars("api"), 0.0).unwrap().0;
        let long = fuzzy(&chars("api-gateway-service"), &chars("api"), 0.0).unwrap().0;
        assert!(short > long);
        // "ab" in "ab": 2 chars + boundary at 0 + proximity 2/√1 = 5, density 1, length 10/12
        let s = fuzzy(&chars("ab"), &chars("ab"), 0.0).unwrap().0;
        assert!((s - 5.0 * 10.0 / 12.0).abs() < 1e-9);
    }

    #[test]
    fn git_uris() {
        let p = |u| parse_git_uri(u).map(|(a, b)| format!("{a}/{b}"));
        assert_eq!(p("https://github.com/tobi/try.git").as_deref(), Some("tobi/try"));
        assert_eq!(p("https://github.com/tobi/try").as_deref(), Some("tobi/try"));
        assert_eq!(p("git@github.com:tobi/try.git").as_deref(), Some("tobi/try"));
        assert_eq!(p("https://gitlab.com/group/proj").as_deref(), Some("group/proj"));
        assert_eq!(p("git@host.xz:team/sub/repo.git").as_deref(), Some("team/repo"));
        assert_eq!(p("ssh://git@host:2222/team/repo.git").as_deref(), Some("team/repo"));
        assert_eq!(p("me@host:path/repo").as_deref(), Some("me/repo"));
        assert_eq!(p("notaurl"), None);
        assert!(is_git_uri("https://x/y/z") && is_git_uri("foo.git") && !is_git_uri("project"));
    }

    #[test]
    fn pr_urls() {
        let pr = github_pr("https://github.com/tobi/try/pull/123/").unwrap();
        assert_eq!((pr.user.as_str(), pr.repo.as_str(), pr.id.as_str()), ("tobi", "try", "123"));
        assert_eq!(pr.git_uri(), "https://github.com/tobi/try.git");
        assert!(github_pr("https://www.github.com/a/b/pull/1").is_some());
        assert!(github_pr("https://github.com/a/b/pull/x").is_none());
        assert!(github_pr("https://github.com/a/b").is_none());
    }

    #[test]
    fn names() {
        assert_eq!(dashify("my  new\tthing"), "my-new-thing");
        assert!(is_date_prefixed("2024-01-15-x") && !is_date_prefixed("2024-01-15") && !is_date_prefixed("abcd-ef-gh-x"));
        assert_eq!(strip_date("2024-01-15-redis"), "redis");
        assert_eq!(q("it's"), r#"'it'"'"'s'"#);
    }

    #[test]
    fn versioning() {
        let d = tmp("ver");
        let date = "2025-01-02";
        assert_eq!(versioned_name(&d, date, "app"), "app");
        fs::create_dir(d.join("2025-01-02-app")).unwrap();
        fs::create_dir(d.join("2025-01-02-app-2")).unwrap();
        assert_eq!(versioned_name(&d, date, "app"), "app-3");
        fs::create_dir(d.join("2025-01-02-v1")).unwrap();
        fs::create_dir(d.join("2025-01-02-v2")).unwrap();
        assert_eq!(versioned_name(&d, date, "v1"), "v3");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn scripts() {
        let s = emit(&script_worktree(Path::new("/t/2025-01-02-x"), Path::new("/r")));
        assert!(s.starts_with(SCRIPT_WARNING));
        assert!(s.contains("{ git -C '/r' worktree add --detach '/t/2025-01-02-x' >/dev/null 2>&1 || true; } && \\\n"));
        assert!(s.trim_end().ends_with("cd '/t/2025-01-02-x'"));
        let pr = github_pr("https://github.com/a/b/pull/7").unwrap();
        let s = script_clone_pr(Path::new("/t/p"), &pr).join("\n");
        assert!(s.contains("fetch origin 'pull/7/head'") && s.contains("checkout --detach FETCH_HEAD"));
        let f = init_function(Path::new("/bin/try"), Some("/x/tries"));
        assert!(f.contains("out=$(TRY_PATH='/x/tries' '/bin/try' \"$@\" 2>/dev/tty) || return"));
    }

    #[test]
    fn graduate_and_delete_scripts() {
        let d = tmp("grad");
        let src = d.join("2025-01-02-app");
        fs::create_dir(&src).unwrap();
        let o = Outcome::Graduate { source: src.clone(), dest: d.join("app"), name: "2025-01-02-app".into() };
        let s = outcome_script(o, &d).join("\n");
        assert!(s.starts_with("mv -- "));
        fs::write(src.join(".git"), "gitdir: x").unwrap();
        let o = Outcome::Graduate { source: src.clone(), dest: d.join("app"), name: "2025-01-02-app".into() };
        assert!(outcome_script(o, &d)[0].starts_with("git worktree move "));

        let sel = Selector::new(d.clone(), "");
        let outside = tmp("outside");
        assert!(sel.validate_delete(&[(outside.clone(), "x".into())]).is_err());
        assert!(sel.validate_delete(&[(d.clone(), "x".into())]).is_err());
        assert!(sel.validate_delete(&[(src, "2025-01-02-app".into())]).is_ok());
        fs::remove_dir_all(&d).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn input_editing() {
        let mut i = Input::new("hello world");
        assert!(i.handle("\x17"));
        assert_eq!(i.text(), "hello ");
        i.handle("\x01");
        i.handle("X");
        assert_eq!(i.text(), "Xhello ");
        i.handle("\x0b");
        assert_eq!(i.text(), "X");
        i.handle("\x1b[D");
        i.handle("\x1b[3~");
        assert_eq!(i.text(), "");
        assert!(!i.handle("\x04") && !i.handle("\r") && !i.handle("\x1b[A"));
    }

    #[test]
    fn layout() {
        assert_eq!(visible_width(&dim("abc")), 3);
        assert_eq!(visible_width("日本"), 4);
        let t = truncate(&highlight("abcdefgh"), 5);
        assert_eq!(visible_width(&t), 5);
        assert!(t.ends_with(&format!("…{RESET_FG}{RESET_BOLD}")));
        let l = Line { text: "name".into(), right: "2h ago, 3.1".into(), ..Default::default() };
        let (s, _) = render_line(&l, 30, false);
        let plain = s.replace(|c: char| c == '\x1b', "");
        assert!(plain.contains("name") && plain.contains("2h ago, 3.1"));
        assert_eq!(visible_width(&s.replace('\r', "")), 29);
        let (_, cur) = render_line(&Line { text: "ab".into(), cursor: Some(1), center: true, ..Default::default() }, 11, true);
        assert_eq!(cur, Some(6));
    }
}
