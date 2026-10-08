//! gray-skills — skill hub browser/installer.
//!
//! Original design (not a port): `skills_search` probes real sources — the
//! npm registry (`keywords:agent-skills` via `/-/v1/search`), GitHub
//! (`topic:agent-skills` repos + `path:SKILL.md` code, via `gh search`), and
//! the `skills` ecosystem CLI (`npx -y skills search`, 15s, silent degrade).
//! `skills_install` fetches a skill pack — npm tarball, git sparse checkout,
//! or a raw SKILL.md — into `~/.gray/skills/<name>/`, refusing packs whose
//! SKILL.md has no frontmatter. `skills_list` / `/skills` show what's there.
//!
//! All network goes through curl/gh/git with 20s timeouts; npm pkgs resolve
//! through the registry so `npm` itself isn't required to install.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const NET_SECS: &str = "20";
const CLI_SECS: &str = "15";
const MAX_TARBALL: usize = 50 * 1024 * 1024;
const MAX_OUT: usize = 7 * 1024;
const PER_SOURCE: usize = 10;
const UA: &str = "gray-skills/0.1 (+https://github.com/vstaln/grayplugins)";

fn manifest() -> Value {
    json!({
        "name": "skills",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "1.1",
        "tools": [
            {
                "name": "skills_search",
                "description": "Search real skill sources for agent skills: npm packages tagged keywords:agent-skills, GitHub repos (topic:agent-skills) and SKILL.md files, and the skills.sh ecosystem CLI. Returns 'name [source] — description' rows; feed a row's name to skills_install.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "What to search for." },
                        "source": { "type": "string", "enum": ["all", "npm", "github", "cli"],
                                    "description": "Which source to probe (default all)." }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "skills_install",
                "description": "Install a skill pack into ~/.gray/skills/<name>/. Source may be an npm package name (or npm:<name>), owner/repo or owner/repo/sub/dir for GitHub (also gh:…, https://github.com/…, owner/repo@skill), or a URL to a raw SKILL.md. Refuses packs with no SKILL.md frontmatter.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "source": { "type": "string", "description": "npm name, owner/repo[/path], owner/repo@skill, or raw SKILL.md URL." }
                    },
                    "required": ["source"]
                }
            },
            {
                "name": "skills_list",
                "description": "List skills installed under ~/.gray/skills/ with their one-line descriptions.",
                "parameters": { "type": "object", "properties": {} }
            }
        ],
        "commands": ["/skills"],
    })
}

/// Installed skills live in the shared gray skills dir ($GRAY_HOME/skills or
/// ~/.gray/skills) — the explicit write target for this plugin.
fn skills_dir() -> PathBuf {
    let home = std::env::var_os("GRAY_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".gray")))
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("skills")
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// GET a URL through curl (20s cap, follow redirects). Returns the body.
fn get(url: &str) -> Result<String, String> {
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            NET_SECS,
            "--compressed",
            "-A",
            UA,
            "--",
            url,
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let detail = err.lines().last().map(str::trim).unwrap_or("curl failed");
        return Err(format!("fetch {url}: {detail}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// GET a URL into a file (for tarballs / raw SKILL.md).
fn get_to(url: &str, dest: &Path, max_bytes: usize) -> Result<(), String> {
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            NET_SECS,
            "--max-filesize",
            &max_bytes.to_string(),
            "-A",
            UA,
            "-o",
        ])
        .arg(dest)
        .arg("--")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let detail = err.lines().last().map(str::trim).unwrap_or("curl failed");
        return Err(format!("fetch {url}: {detail}"));
    }
    Ok(())
}

/// Run `prog args…` under `timeout <secs>`; return stdout on success.
fn run(secs: &str, prog: &str, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("timeout")
        .arg(secs)
        .arg(prog)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("TERM", "dumb")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("couldn't run {prog}: {e}"))?;
    if out.status.code() == Some(124) {
        return Err(format!("{prog} timed out after {secs}s"));
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let detail = err.lines().last().map(str::trim).unwrap_or("failed");
        return Err(format!("{prog}: {detail}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: ESC [ ... final byte in @-~ ; also bare ESC sequences.
            if chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    if ('@'..='~').contains(&c2) {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\x07' {
            out.push(c);
        }
    }
    out
}

/// Parse YAML-ish frontmatter: file must open with a `---` line and close
/// with `---` or `...`. Returns (name, description) — either may be missing.
fn frontmatter(text: &str) -> Option<(Option<String>, Option<String>)> {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines();
    if !matches!(lines.next().map(str::trim_end), Some("---")) {
        return None;
    }
    let mut name = None;
    let mut desc = None;
    let mut closed = false;
    for line in lines.take(80) {
        let t = line.trim_end();
        if t == "---" || t == "..." {
            closed = true;
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            match k.trim() {
                "name" if name.is_none() => name = Some(v),
                "description" if desc.is_none() => desc = Some(v),
                _ => {}
            }
        }
    }
    closed.then_some((name, desc))
}

/// Filesystem-safe skill dir name: lowercase alnum + - _ ., else '-'.
fn safe_name(s: &str) -> String {
    let mut n = String::with_capacity(s.len().min(64));
    for c in s.trim().to_lowercase().chars() {
        let c = if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
            c
        } else {
            '-'
        };
        // collapse runs of separators so `../` can't survive inside a name
        if c == '-' && n.ends_with('-') {
            continue;
        }
        n.push(c);
    }
    n.trim_matches(|c| c == '-' || c == '.' || c == '_')
        .chars()
        .take(64)
        .collect()
}

/// Walk `root` (depth ≤ 5) for dirs containing a SKILL.md with frontmatter.
fn find_skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 5 || out.len() >= 25 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !e.file_name().to_string_lossy().starts_with('.') {
                    stack.push((p, depth + 1));
                }
            } else if ft.is_file()
                && e.file_name().to_string_lossy().eq_ignore_ascii_case("skill.md")
                && std::fs::read_to_string(&p)
                    .ok()
                    .and_then(|t| frontmatter(&t))
                    .is_some()
            {
                out.push(dir.clone());
            }
        }
    }
    out.sort();
    out
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let ft = e.file_type()?;
        let target = dst.join(e.file_name());
        if ft.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else if ft.is_file() {
            std::fs::copy(e.path(), &target)?;
        }
    }
    Ok(())
}

/// A search hit: display name, which source produced it, one-line detail.
struct Row {
    name: String,
    source: &'static str,
    desc: String,
}

fn clip(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > n {
        let t: String = s.chars().take(n).collect();
        format!("{t}…")
    } else {
        s
    }
}

fn search_npm(query: &str) -> Result<Vec<Row>, String> {
    let text = if query.is_empty() {
        "keywords:agent-skills".to_string()
    } else {
        format!("keywords:agent-skills {query}")
    };
    let url = format!(
        "https://registry.npmjs.org/-/v1/search?text={}&size={PER_SOURCE}",
        url_encode(&text)
    );
    let body = get(&url)?;
    let v: Value = serde_json::from_str(&body).map_err(|e| format!("npm: bad json: {e}"))?;
    let mut rows = Vec::new();
    for o in v["objects"].as_array().into_iter().flatten() {
        let p = &o["package"];
        let name = p["name"].as_str().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        let ver = p["version"].as_str().unwrap_or("");
        let desc = p["description"].as_str().unwrap_or("");
        rows.push(Row {
            name: format!("{name}@{ver}"),
            source: "npm",
            desc: clip(desc, 110),
        });
        if rows.len() >= PER_SOURCE {
            break;
        }
    }
    Ok(rows)
}

fn search_github(query: &str) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    let mut errs = Vec::new();
    let q = if query.is_empty() { "agent skills" } else { query };
    match run(
        NET_SECS,
        "gh",
        &[
            "search",
            "repos",
            &format!("{q} topic:agent-skills"),
            "--json",
            "fullName,description",
            "--limit",
            "6",
        ],
    )
    .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| format!("bad json: {e}")))
    {
        Ok(v) => {
            for r in v.as_array().into_iter().flatten() {
                let name = r["fullName"].as_str().unwrap_or("").to_string();
                if name.is_empty() {
                    continue;
                }
                rows.push(Row {
                    name,
                    source: "github",
                    desc: clip(r["description"].as_str().unwrap_or(""), 110),
                });
            }
        }
        Err(e) => errs.push(format!("repos {e}")),
    }
    match run(
        NET_SECS,
        "gh",
        &[
            "search",
            "code",
            &format!("{q} path:SKILL.md"),
            "--json",
            "repository,path",
            "--limit",
            "6",
        ],
    )
    .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| format!("bad json: {e}")))
    {
        Ok(v) => {
            for r in v.as_array().into_iter().flatten() {
                let repo = r["repository"]["nameWithOwner"].as_str().unwrap_or("");
                let path = r["path"].as_str().unwrap_or("");
                if repo.is_empty() {
                    continue;
                }
                // Installable name: repo plus the SKILL.md's parent dir.
                let dir = Path::new(path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                let name = if dir.is_empty() || dir == "." {
                    repo.to_string()
                } else {
                    format!("{repo}/{dir}")
                };
                rows.push(Row {
                    name,
                    source: "github-code",
                    desc: format!("SKILL.md at {path}"),
                });
            }
        }
        Err(e) => errs.push(format!("code {e}")),
    }
    if rows.is_empty() && !errs.is_empty() {
        return Err(errs.join("; "));
    }
    Ok(rows)
}

/// `npx -y skills search` — output is ANSI rows like
/// `owner/repo@skill  12K installs` + `└ https://skills.sh/…`.
/// Degrades silently: any failure just contributes no rows.
fn search_cli(query: &str) -> Vec<Row> {
    let Ok(raw) = run(CLI_SECS, "npx", &["-y", "skills", "search", query]) else {
        return Vec::new();
    };
    let clean = strip_ansi(&raw);
    let mut rows: Vec<Row> = Vec::new();
    for line in clean.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix('└') {
            if let Some(last) = rows.last_mut() {
                last.desc = clip(rest.trim(), 110);
            }
            continue;
        }
        let mut it = t.split_whitespace();
        let Some(name) = it.next() else { continue };
        if !(name.contains('/') && name.contains('@')) {
            continue;
        }
        rows.push(Row {
            name: name.to_string(),
            source: "cli",
            desc: clip(&it.collect::<Vec<_>>().join(" "), 110),
        });
        if rows.len() >= PER_SOURCE {
            break;
        }
    }
    rows
}

fn tool_search(args: &Value) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        return Err("missing required argument: query".into());
    }
    let source = args
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("all")
        .to_lowercase();
    if !matches!(source.as_str(), "all" | "npm" | "github" | "cli") {
        return Err(format!("unknown source: {source} — all|npm|github|cli"));
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    if matches!(source.as_str(), "all" | "npm") {
        match search_npm(&query) {
            Ok(r) => rows.extend(r),
            Err(e) => notes.push(format!("npm failed: {e}")),
        }
    }
    if matches!(source.as_str(), "all" | "github") {
        match search_github(&query) {
            Ok(r) => rows.extend(r),
            Err(e) => notes.push(format!("github failed: {e}")),
        }
    }
    if matches!(source.as_str(), "all" | "cli") {
        rows.extend(search_cli(&query));
    }
    let mut out = format!("skills search “{query}” — {} hit(s)\n", rows.len());
    for r in &rows {
        out.push_str(&format!("- {} [{}]", r.name, r.source));
        if !r.desc.is_empty() {
            out.push_str(&format!(" — {}", r.desc));
        }
        out.push('\n');
    }
    for n in &notes {
        out.push_str(&format!("(note: {n})\n"));
    }
    if !rows.is_empty() {
        out.push_str("install one: skills_install {source: \"<name from a row>\"}");
    } else if notes.is_empty() {
        out.push_str("no hits — try broader terms or source: \"npm\"|\"github\"|\"cli\"");
    }
    out.truncate(MAX_OUT);
    Ok(out)
}

/// What `skills_install` was pointed at.
enum Source {
    Npm(String),
    /// clone_url + optional in-repo subdir + optional skill-name hint (@skill).
    Git { url: String, subdir: Option<String>, hint: Option<String> },
    /// A single raw SKILL.md.
    Raw(String),
}

fn classify(source: &str) -> Result<Source, String> {
    let s = source.trim();
    if s.is_empty() {
        return Err("missing required argument: source".into());
    }
    if let Some(rest) = s.strip_prefix("npm:") {
        return Ok(Source::Npm(rest.trim().to_string()));
    }
    if let Some(rest) = s.strip_prefix("raw:") {
        return Ok(Source::Raw(rest.trim().to_string()));
    }
    let s = s
        .strip_prefix("gh:")
        .or_else(|| s.strip_prefix("github:"))
        .unwrap_or(s);
    if s.starts_with("http://") || s.starts_with("https://") {
        // github.com/owner/repo[/tree|blob/<branch>/<path>] or a raw file URL.
        let after = s
            .splitn(2, "github.com/")
            .nth(1)
            .map(|t| t.trim_end_matches('/').to_string());
        if let Some(rest) = after.as_deref() {
            let parts: Vec<&str> = rest.split('/').collect();
            if parts.len() >= 2 {
                let repo_url =
                    format!("https://github.com/{}/{}.git", parts[0], parts[1].trim_end_matches(".git"));
                if parts.len() > 4 && (parts[2] == "blob" || parts[2] == "tree") {
                    let path = parts[4..].join("/");
                    if parts[2] == "blob" {
                        if !path.to_lowercase().ends_with("skill.md") {
                            return Err(format!(
                                "github blob URL must point at a SKILL.md (got …/{path})"
                            ));
                        }
                        return Ok(Source::Raw(format!(
                            "https://raw.githubusercontent.com/{}/{}/{}/{}",
                            parts[0], parts[1], parts[3], path
                        )));
                    }
                    return Ok(Source::Git {
                        url: repo_url,
                        subdir: Some(path),
                        hint: None,
                    });
                }
                return Ok(Source::Git {
                    url: repo_url,
                    subdir: None,
                    hint: None,
                });
            }
        }
        // Non-github URL or bare repo URL: must end at a SKILL.md to fetch raw.
        if s.to_lowercase().ends_with("skill.md") {
            return Ok(Source::Raw(s.to_string()));
        }
        if let Some(rest) = after {
            // https://github.com/owner only — not enough.
            return Err(format!("github URL needs owner/repo (got …{rest})"));
        }
        return Err(format!(
            "unsupported URL: {s} — expected github.com/owner/repo or a raw SKILL.md URL"
        ));
    }
    if let Some(rest) = s.strip_prefix('@') {
        // npm scoped package.
        return Ok(Source::Npm(format!("@{rest}")));
    }
    if s.contains('/') {
        // owner/repo[/sub/dir] or owner/repo@skill
        let mut parts = s.splitn(3, '/');
        let owner = parts.next().unwrap_or("");
        let repo_part = parts.next().unwrap_or("");
        if owner.is_empty() || repo_part.is_empty() {
            return Err(format!("github source needs owner/repo (got {s})"));
        }
        let (repo, hint) = match repo_part.split_once('@') {
            Some((r, h)) => (r, Some(h.to_string())),
            None => (repo_part, None),
        };
        let subdir = parts.next().map(str::to_string);
        return Ok(Source::Git {
            url: format!("https://github.com/{owner}/{repo}.git"),
            subdir,
            hint,
        });
    }
    Ok(Source::Npm(s.to_string()))
}

/// Fetch an npm pack's tarball into `staging` and extract it.
fn fetch_npm(staging: &Path, name: &str) -> Result<(), String> {
    let reg_name = name.replace('/', "%2f");
    let meta = get(&format!("https://registry.npmjs.org/{reg_name}"))?;
    let v: Value = serde_json::from_str(&meta).map_err(|e| format!("npm: bad json: {e}"))?;
    let latest = v["dist-tags"]["latest"]
        .as_str()
        .ok_or_else(|| format!("npm: no dist-tags.latest for {name}"))?;
    let tar = v["versions"][latest]["dist"]["tarball"]
        .as_str()
        .ok_or_else(|| format!("npm: no tarball URL for {name}@{latest}"))?
        .to_string();
    let tgz = staging.join("pack.tgz");
    get_to(&tar, &tgz, MAX_TARBALL)?;
    let out = std::process::Command::new("tar")
        .arg("xzf")
        .arg(&tgz)
        .arg("-C")
        .arg(staging)
        .output()
        .map_err(|e| format!("couldn't run tar: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "tar failed on {name}@{latest}: {}",
            String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("")
        ));
    }
    Ok(())
}

/// Clone a repo into `staging` — sparse when a subdir was named.
fn fetch_git(staging: &Path, url: &str, subdir: Option<&str>) -> Result<(), String> {
    let dest = staging.join("repo");
    let mut args = vec!["clone", "--depth", "1"];
    if subdir.is_some() {
        args.push("--filter=blob:none");
        args.push("--sparse");
    }
    args.push(url);
    let dest_s = dest.to_string_lossy().to_string();
    args.push(&dest_s);
    run("40", "git", &args)?; // clones can exceed the 20s GET budget once
    if let Some(sub) = subdir {
        run(NET_SECS, "git", &["-C", &dest_s, "sparse-checkout", "set", sub])?;
    }
    Ok(())
}

/// After a fetch fills `staging`, find every SKILL.md-frontmatter dir and
/// copy it into skills_dir. Returns report lines. Refuses when none qualify.
fn install_staged(staging: &Path, hint: Option<&str>) -> Result<Vec<String>, String> {
    let dirs = find_skill_dirs(staging);
    if dirs.is_empty() {
        return Err("no SKILL.md with frontmatter found in that pack".into());
    }
    // A @skill hint narrows a multi-skill pack to the named one.
    let dirs: Vec<PathBuf> = match hint {
        Some(h) if dirs.len() > 1 => {
            let hl = h.to_lowercase();
            let narrowed: Vec<PathBuf> = dirs
                .iter()
                .filter(|d| {
                    d.file_name()
                        .map(|n| n.to_string_lossy().to_lowercase() == hl)
                        .unwrap_or(false)
                })
                .cloned()
                .collect();
            if narrowed.is_empty() {
                dirs
            } else {
                narrowed
            }
        }
        _ => dirs,
    };
    let dest_root = skills_dir();
    std::fs::create_dir_all(&dest_root).map_err(|e| format!("can't create {dest_root:?}: {e}"))?;
    let mut report = Vec::new();
    for dir in dirs.iter().take(20) {
        let md = std::fs::read_dir(dir).ok().and_then(|mut it| {
            it.find_map(|e| {
                let e = e.ok()?;
                e.file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("skill.md")
                    .then(|| e.path())
            })
        });
        let fm = md
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| frontmatter(&t));
        let raw_name = fm
            .as_ref()
            .and_then(|(n, _)| n.clone())
            .or_else(|| {
                dir.file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .unwrap_or_default();
        let name = safe_name(&raw_name);
        if name.is_empty() {
            report.push(format!("- skipped {}: unusable name", dir.display()));
            continue;
        }
        let dest = dest_root.join(&name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)
                .map_err(|e| format!("can't replace {}: {e}", dest.display()))?;
        }
        copy_dir(dir, &dest).map_err(|e| format!("copy {} failed: {e}", dir.display()))?;
        let desc = fm.and_then(|(_, d)| d).unwrap_or_default();
        report.push(format!(
            "- {name} → {}{}",
            dest.display(),
            if desc.is_empty() {
                String::new()
            } else {
                format!(" — {}", clip(&desc, 90))
            }
        ));
    }
    Ok(report)
}

fn tool_install(args: &Value) -> Result<String, String> {
    let src = classify(
        args.get("source").and_then(Value::as_str).unwrap_or(""),
    )?;
    let staging = skills_dir().join(format!(".staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("can't stage in ~/.gray/skills: {e}"))?;
    let cleanup = |staging: &Path| {
        let _ = std::fs::remove_dir_all(staging);
    };
    let hint = match &src {
        Source::Git { hint, .. } => hint.clone(),
        _ => None,
    };
    let fetched: Result<(), String> = match &src {
        Source::Npm(name) => fetch_npm(&staging, name),
        Source::Git { url, subdir, .. } => fetch_git(&staging, url, subdir.as_deref()),
        Source::Raw(url) => {
            let dir = staging.join("raw");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            get_to(url, &dir.join("SKILL.md"), 1024 * 1024)
        }
    };
    if let Err(e) = fetched {
        cleanup(&staging);
        return Err(e);
    }
    let report = install_staged(&staging, hint.as_deref());
    cleanup(&staging);
    let lines = report?;
    let mut out = format!("installed {} skill(s):\n", lines.len());
    out.push_str(&lines.join("\n"));
    out.truncate(MAX_OUT);
    Ok(out)
}

fn list_skills(dir: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return format!("no skills installed yet ({} doesn't exist)", dir.display());
    };
    let mut rows = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || !e.path().is_dir() {
            continue;
        }
        let md = e.path().join("SKILL.md");
        let alt = e.path().join("skill.md");
        let md = if md.exists() { md } else { alt };
        match std::fs::read_to_string(&md)
            .ok()
            .and_then(|t| frontmatter(&t))
        {
            Some((n, d)) => rows.push(format!(
                "- {} — {}",
                n.unwrap_or(name),
                clip(&d.unwrap_or_default(), 110)
            )),
            None => rows.push(format!("- {name} (no SKILL.md frontmatter)")),
        }
    }
    if rows.is_empty() {
        return format!("no skills installed under {}", dir.display());
    }
    rows.sort();
    format!("{} skill(s) in {}:\n{}", rows.len(), dir.display(), rows.join("\n"))
}

fn call_tool(name: &str, args: &Value) -> Result<String, String> {
    match name {
        "skills_search" => tool_search(args),
        "skills_install" => tool_install(args),
        "skills_list" => Ok(list_skills(&skills_dir())),
        other => Err(format!("unknown tool: {other}")),
    }
}

/// `/skills …` — bare lists; also `search <q>` / `install <src>` passthroughs.
fn run_command(argv: &[&str]) -> String {
    match argv.first().copied() {
        None | Some("list") | Some("ls") => list_skills(&skills_dir()),
        Some("search") if argv.len() > 1 => {
            match tool_search(&json!({ "query": argv[1..].join(" ") })) {
                Ok(t) | Err(t) => t,
            }
        }
        Some("install") if argv.len() > 1 => {
            match tool_install(&json!({ "source": argv[1] })) {
                Ok(t) | Err(t) => t,
            }
        }
        Some(other) => format!(
            "unknown /skills subcommand: {other} — try /skills, /skills search <q>, /skills install <src>"
        ),
    }
}

fn handle(req: &Value) -> (Option<Value>, bool) {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        return (None, method == "plugin/shutdown");
    };
    let result = match method {
        "plugin/manifest" => manifest(),
        "tool/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("args").cloned().unwrap_or(Value::Null);
            match call_tool(name, &args) {
                Ok(text) => json!({ "content": text }),
                Err(text) => json!({ "content": text, "is_error": true }),
            }
        }
        "command/run" => {
            let argv: Vec<&str> = params
                .get("argv")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            json!({ "text": run_command(&argv) })
        }
        "plugin/shutdown" => return (Some(json!({ "id": id, "result": {} })), true),
        _ => {
            let error = json!({ "code": -32601, "message": "method not found" });
            return (Some(json!({ "id": id, "error": error })), false);
        }
    };
    (Some(json!({ "id": id, "result": result })), false)
}

fn main() -> std::io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("manifest") {
        println!("{}", manifest());
        return Ok(());
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let (reply, exit) = handle(&req);
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
        if exit {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(method: &str, params: Value) -> Value {
        handle(&json!({ "id": 1, "method": method, "params": params }))
            .0
            .unwrap()
    }

    #[test]
    fn manifest_has_tools_and_command() {
        let m = call("plugin/manifest", Value::Null)["result"].clone();
        assert_eq!(m["name"], "skills");
        assert_eq!(m["commands"], json!(["/skills"]));
        let tools: Vec<&str> = m["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(tools, ["skills_search", "skills_install", "skills_list"]);
    }

    #[test]
    fn frontmatter_parses_name_and_description() {
        let t = "---\nname: my-skill\ndescription: does a thing\n---\n# body\n";
        let (n, d) = frontmatter(t).unwrap();
        assert_eq!(n.as_deref(), Some("my-skill"));
        assert_eq!(d.as_deref(), Some("does a thing"));
        assert!(frontmatter("# no frontmatter").is_none());
        assert!(frontmatter("---\nname: open\n").is_none()); // unclosed
        let (n2, _) = frontmatter("---\ndescription: only desc\n---\n").unwrap();
        assert!(n2.is_none());
    }

    #[test]
    fn safe_name_sanitizes() {
        assert_eq!(safe_name("My Skill!"), "my-skill");
        assert_eq!(safe_name("..evil../x"), "evil-x");
        assert_eq!(safe_name("___"), "");
        assert_eq!(safe_name("code_review"), "code_review");
    }

    #[test]
    fn ansi_strips() {
        assert_eq!(strip_ansi("\x1b[1mhi\x1b[0m!"), "hi!");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn classify_routes_sources() {
        assert!(matches!(classify("leftpad").unwrap(), Source::Npm(_)));
        assert!(matches!(
            classify("@scope/pkg").unwrap(),
            Source::Npm(_)
        ));
        assert!(matches!(
            classify("npm:pkg").unwrap(),
            Source::Npm(_)
        ));
        match classify("owner/repo").unwrap() {
            Source::Git { url, subdir, hint } => {
                assert_eq!(url, "https://github.com/owner/repo.git");
                assert!(subdir.is_none() && hint.is_none());
            }
            _ => panic!("owner/repo should be git"),
        }
        match classify("owner/repo/skills/foo").unwrap() {
            Source::Git { subdir, .. } => assert_eq!(subdir.as_deref(), Some("skills/foo")),
            _ => panic!(),
        }
        match classify("owner/repo@code-review").unwrap() {
            Source::Git { hint, .. } => assert_eq!(hint.as_deref(), Some("code-review")),
            _ => panic!(),
        }
        match classify("https://github.com/o/r/tree/main/skills/x").unwrap() {
            Source::Git { url, subdir, .. } => {
                assert_eq!(url, "https://github.com/o/r.git");
                assert_eq!(subdir.as_deref(), Some("skills/x"));
            }
            _ => panic!(),
        }
        match classify("https://github.com/o/r/blob/main/SKILL.md").unwrap() {
            Source::Raw(u) => {
                assert_eq!(u, "https://raw.githubusercontent.com/o/r/main/SKILL.md")
            }
            _ => panic!(),
        }
        assert!(matches!(
            classify("https://example.com/x/SKILL.md").unwrap(),
            Source::Raw(_)
        ));
        assert!(classify("https://example.com/not-a-skill.txt").is_err());
        assert!(classify("").is_err());
    }

    static FIXTURE_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn fixture() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gray-skills-test-{}-{}",
            std::process::id(),
            FIXTURE_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn find_skill_dirs_needs_frontmatter() {
        let root = fixture();
        let good = root.join("pack/skills/good");
        let bad = root.join("pack/skills/bad");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(good.join("SKILL.md"), "---\nname: good\n---\n").unwrap();
        std::fs::write(bad.join("SKILL.md"), "no frontmatter here\n").unwrap();
        let found = find_skill_dirs(&root);
        assert_eq!(found, vec![good]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_staged_copies_named_skills() {
        let root = fixture();
        let staging = root.join("staging");
        let sk = staging.join("package");
        std::fs::create_dir_all(&sk).unwrap();
        std::fs::write(
            sk.join("SKILL.md"),
            "---\nname: Test Skill\ndescription: hi there\n---\n",
        )
        .unwrap();
        std::fs::write(sk.join("extra.txt"), "x").unwrap();
        // install_staged writes to skills_dir(); redirect via a temp GRAY_HOME.
        // The env var is process-global — hold the lock so parallel tests
        // can't repoint it mid-install.
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("GRAY_HOME", root.join("grayhome"));
        }
        let report = install_staged(&staging, None).unwrap();
        assert_eq!(report.len(), 1);
        let dest = root.join("grayhome/skills/test-skill");
        assert!(dest.join("SKILL.md").exists());
        assert!(dest.join("extra.txt").exists());
        assert!(report[0].contains("test-skill"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_staged_refuses_no_frontmatter() {
        let root = fixture();
        let staging = root.join("staging");
        std::fs::create_dir_all(staging.join("pkg")).unwrap();
        std::fs::write(staging.join("pkg/SKILL.md"), "plain markdown").unwrap();
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("GRAY_HOME", root.join("grayhome"));
        }
        let err = install_staged(&staging, None).unwrap_err();
        assert!(err.contains("no SKILL.md"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_skills_reports_empty_and_installed() {
        let root = fixture();
        assert!(list_skills(&root.join("nope")).contains("no skills"));
        let sk = root.join("skills/alpha");
        std::fs::create_dir_all(&sk).unwrap();
        std::fs::write(
            sk.join("SKILL.md"),
            "---\nname: alpha\ndescription: first\n---\n",
        )
        .unwrap();
        let out = list_skills(&root.join("skills"));
        assert!(out.contains("alpha — first"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tool_errors_are_flagged() {
        let r = call("tool/call", json!({ "name": "skills_search", "args": {} }));
        assert_eq!(r["result"]["is_error"], true);
        let r = call(
            "tool/call",
            json!({ "name": "skills_search", "args": { "query": "x", "source": "nope" } }),
        );
        assert!(r["result"]["content"].as_str().unwrap().contains("unknown source"));
        let r = call("tool/call", json!({ "name": "nope", "args": {} }));
        assert!(r["result"]["content"].as_str().unwrap().contains("unknown tool"));
    }

    #[test]
    fn unknown_methods_are_method_not_found() {
        assert_eq!(call("nope", Value::Null)["error"]["code"], -32601);
    }

    #[test]
    fn shutdown_replies_then_exits_and_notifications_are_silent() {
        let (reply, exit) = handle(&json!({ "id": 2, "method": "plugin/shutdown" }));
        assert!(reply.is_some() && exit);
        let (reply, exit) = handle(&json!({ "method": "plugin/shutdown" }));
        assert!(reply.is_none() && exit);
    }
}
