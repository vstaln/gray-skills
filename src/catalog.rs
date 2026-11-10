//! Skill discovery and the listing the model sees. Claude Code's scheme: every
//! skill name always appears; descriptions share a budget of 1% of the context
//! window, split evenly from the most-used down, so a crowded catalog clips
//! descriptions instead of dropping them. The `skill` tool loads a
//! body by name, so the listing carries no paths.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const MAX_DESCRIPTION_CHARS: usize = 1536;
const MIN_DESCRIPTION_CHARS: usize = 24;
const DEFAULT_CONTEXT_WINDOW: usize = 200_000;
const CHARS_PER_TOKEN: usize = 4;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// `disable-model-invocation`: runs from `/skills <name>` only.
    pub user_only: bool,
    pub args: Vec<String>,
}

impl Skill {
    pub fn dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }
}

#[derive(Debug, Default)]
struct Front {
    name: Option<String>,
    description: Option<String>,
    user_only: bool,
    args: Vec<String>,
}

/// Splits frontmatter from the body. `None` means it was never closed.
fn split_front(content: &str) -> Option<(Front, &str)> {
    let trimmed = content.trim_start_matches('\u{feff}').trim_start();
    let Some(after) = trimmed.strip_prefix("---") else {
        return Some((Front::default(), content));
    };
    let after = after
        .strip_prefix("\r\n")
        .or_else(|| after.strip_prefix('\n'))
        .unwrap_or(after);
    let mut offset = 0;
    for line in after.split_inclusive('\n') {
        if line.trim() == "---" {
            return Some((parse_front(&after[..offset]), &after[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

fn parse_front(block: &str) -> Front {
    let mut fm = Front::default();
    let lines: Vec<&str> = block.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        i += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().trim_matches(['"', '\'']);
        let mut val = val.trim().to_string();
        let folded = matches!(val.as_str(), ">" | ">-" | ">+");
        if folded || matches!(val.as_str(), "|" | "|-" | "|+") {
            let mut parts = Vec::new();
            while i < lines.len() {
                let next = lines[i];
                if !next.trim().is_empty() {
                    if !next.starts_with([' ', '\t']) {
                        break;
                    }
                    parts.push(next.trim());
                }
                i += 1;
            }
            val = parts.join(if folded { " " } else { "\n" });
        } else if val.len() >= 2
            && ((val.starts_with('"') && val.ends_with('"'))
                || (val.starts_with('\'') && val.ends_with('\'')))
        {
            val = val[1..val.len() - 1].to_string();
        }
        match key {
            "name" => fm.name = Some(val),
            "description" => fm.description = Some(val),
            "disable-model-invocation" | "disable_model_invocation" => {
                fm.user_only = val.eq_ignore_ascii_case("true")
            }
            "args" | "arguments" => {
                fm.args = val
                    .trim_matches(['[', ']'])
                    .split([',', ' ', '\t'])
                    .map(|s| s.trim().trim_matches(['"', '\'']).trim_start_matches('-'))
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            }
            _ => {}
        }
    }
    fm
}

/// The text after the frontmatter.
pub fn body(content: &str) -> &str {
    split_front(content).map_or(content, |(_, b)| b)
}

fn load_skill(path: &Path) -> Option<Skill> {
    let raw = fs::read_to_string(path).ok()?;
    let (fm, _) = split_front(&raw)?;
    let description = fm.description.filter(|d| !d.trim().is_empty())?;
    let dir_name = path
        .parent()?
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    Some(Skill {
        name: fm.name.unwrap_or_else(|| dir_name.to_string()),
        description,
        path: path.to_path_buf(),
        user_only: fm.user_only,
        args: fm.args,
    })
}

/// Stops descending at the first SKILL.md; loose `.md` files count only at the
/// root of a skills dir. Follows symlinks, with `seen` guarding against cycles.
fn walk(dir: &Path, root: bool, seen: &mut HashSet<PathBuf>, out: &mut Vec<Skill>) {
    if !seen.insert(fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())) {
        return;
    }
    let manifest = dir.join("SKILL.md");
    if manifest.is_file() {
        out.extend(load_skill(&manifest));
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            walk(&path, false, seen, out);
        } else if root && name.ends_with(".md") {
            out.extend(load_skill(&path));
        }
    }
}

fn claude_disabled_plugins(home: &Path) -> HashSet<String> {
    let mut disabled = HashSet::new();
    for file in ["settings.json", "settings.local.json"] {
        let json = read_json(&home.join(".claude").join(file));
        let Some(map) = json.get("enabledPlugins").and_then(Value::as_object) else {
            continue;
        };
        for (key, enabled) in map {
            if enabled.as_bool() == Some(false) {
                disabled.insert(key.clone());
            } else {
                disabled.remove(key);
            }
        }
    }
    disabled
}

fn claude_plugin_skill_dirs(home: &Path) -> Vec<PathBuf> {
    let json = read_json(&home.join(".claude/plugins/installed_plugins.json"));
    let Some(plugins) = json.get("plugins").and_then(Value::as_object) else {
        return Vec::new();
    };
    let disabled = claude_disabled_plugins(home);
    let mut dirs = Vec::new();
    for (name, installs) in plugins {
        if disabled.contains(name) {
            continue;
        }
        let latest = installs
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let path = e.get("installPath")?.as_str()?;
                let updated = e
                    .get("lastUpdated")
                    .or_else(|| e.get("installedAt"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                Some((updated, path))
            })
            .max_by_key(|(updated, _)| *updated);
        if let Some((_, path)) = latest {
            dirs.push(Path::new(path).join("skills"));
        }
    }
    dirs
}

fn find_git_root(start: &Path) -> Option<&Path> {
    start.ancestors().find(|d| d.join(".git").exists())
}

/// Search order; the first skill to claim a name wins.
fn roots(cwd: &Path, gray_home: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = vec![gray_home.join("skills")];
    if std::env::var("GRAY_SKILLS_ONLY").as_deref() == Ok("1") {
        return out;
    }
    if let Some(home) = home {
        out.push(home.join(".claude/skills"));
        let config =
            std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
        let opencode = config.join("opencode");
        out.push(opencode.join("skills"));
        if let Ok(entries) = fs::read_dir(&opencode) {
            out.extend(entries.flatten().map(|e| e.path().join("skills")));
        }
        out.push(home.join(".agents/skills"));
        out.extend(claude_plugin_skill_dirs(home));
    }
    let git_root = find_git_root(cwd);
    let mut dirs = Vec::new();
    for dir in cwd.ancestors() {
        dirs.push(dir);
        if Some(dir) == git_root {
            break;
        }
    }
    for dir in dirs.iter().rev() {
        for cfg in [".gray", ".claude", ".opencode", ".agents"] {
            out.push(dir.join(cfg).join("skills"));
        }
    }
    out
}

fn discover_in(cwd: &Path, gray_home: &Path, home: Option<&Path>) -> Vec<Skill> {
    let mut by_name = BTreeMap::new();
    let mut seen = HashSet::new();
    for root in roots(cwd, gray_home, home) {
        let mut found = Vec::new();
        walk(&root, true, &mut seen, &mut found);
        for skill in found {
            by_name.entry(skill.name.clone()).or_insert(skill);
        }
    }
    by_name.into_values().collect()
}

pub fn discover(cwd: &Path) -> Vec<Skill> {
    discover_in(cwd, &crate::gray_home(), std::env::home_dir().as_deref())
}

fn read_json(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null)
}

/// `/skills` preferences and usage counts, kept out of gray's own config.
#[derive(Debug, Default)]
pub struct State {
    pub off: bool,
    pub disabled: HashSet<String>,
    pub uses: HashMap<String, u64>,
}

fn state_path() -> PathBuf {
    crate::gray_home().join("skills-state.json")
}

impl State {
    pub fn load() -> Self {
        Self::from_json(&read_json(&state_path()))
    }

    fn from_json(json: &Value) -> Self {
        Self {
            off: json["off"].as_bool().unwrap_or(false),
            disabled: json["disabled"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            uses: json["uses"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(k, v)| Some((k.clone(), v.as_u64()?)))
                .collect(),
        }
    }

    // ponytail: last writer wins across concurrent gray processes; a lost use
    // count only nudges description order. Lock it if the toggles ever race.
    pub fn save(&self) -> Result<(), String> {
        let path = state_path();
        let tmp = path.with_extension(format!("json.{}", std::process::id()));
        let mut disabled: Vec<&String> = self.disabled.iter().collect();
        disabled.sort();
        let body = json!({ "off": self.off, "disabled": disabled, "uses": self.uses });
        fs::create_dir_all(crate::gray_home())
            .and_then(|()| fs::write(&tmp, body.to_string()))
            .and_then(|()| fs::rename(&tmp, &path))
            .map_err(|e| format!("cannot save {}: {e}", path.display()))
    }

    /// Whether the model may see and load this skill.
    pub fn offered(&self, skill: &Skill) -> bool {
        !self.off && !skill.user_only && !self.disabled.contains(&skill.name)
    }

    pub fn record_use(name: &str) {
        let mut state = Self::load();
        *state.uses.entry(name.to_string()).or_default() += 1;
        let _ = state.save();
    }
}

/// 1% of the context window, in characters. GRAY_SKILL_CHAR_BUDGET overrides.
pub fn char_budget() -> usize {
    if let Some(n) = env_usize("GRAY_SKILL_CHAR_BUDGET") {
        return n;
    }
    let window = env_usize("GRAY_CONTEXT_WINDOW")
        .or_else(|| {
            read_json(&crate::gray_home().join("config.json"))["context_window"]
                .as_u64()
                .map(|n| n as usize)
        })
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);
    window / 100 * CHARS_PER_TOKEN
}

fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key).ok()?.trim().parse().ok()
}

/// The block injected into the prompt; `None` when nothing is on offer.
pub fn listing(skills: &[Skill], state: &State, budget: usize) -> Option<String> {
    let offered: Vec<&Skill> = skills.iter().filter(|s| state.offered(s)).collect();
    if offered.is_empty() {
        return None;
    }
    let uses = |s: &Skill| state.uses.get(&s.name).copied().unwrap_or(0);
    let mut by_use = offered.clone();
    // Shortest first among equals: what a short description leaves of its
    // share passes on to the longer ones.
    by_use.sort_by_key(|s| (std::cmp::Reverse(uses(s)), s.description.len()));

    let names: usize = offered.iter().map(|s| s.name.chars().count() + 3).sum();
    let mut left = budget.saturating_sub(names);
    let mut described = HashMap::new();
    let count = by_use.len();
    for (i, skill) in by_use.into_iter().enumerate() {
        let share = (left / (count - i)).saturating_sub(2);
        if share < MIN_DESCRIPTION_CHARS {
            continue;
        }
        // clip() adds an ellipsis past its limit, so ask for one char less:
        // text + ": " then stays inside this skill's even share of `left`.
        let text = crate::clip(
            &skill.description,
            share.saturating_sub(1).min(MAX_DESCRIPTION_CHARS),
        );
        left = left.saturating_sub(text.chars().count() + 2);
        described.insert(skill.name.as_str(), text);
    }

    let mut out = String::from(
        "Skills: when a task matches one, call the `skill` tool with its name before acting. Skills listed without a description are just as available.\n<available_skills>\n",
    );
    for skill in offered {
        out.push_str("- ");
        out.push_str(&skill.name);
        if let Some(text) = described.get(skill.name.as_str()) {
            out.push_str(": ");
            out.push_str(text);
        }
        out.push('\n');
    }
    out.push_str("</available_skills>");
    Some(out)
}

pub fn validate_args(skill: &Skill, args: &str) -> Result<(), String> {
    if skill.args.is_empty() {
        return Ok(());
    }
    let unknown: Vec<&str> = args
        .split([' ', '\t', ','])
        .map(|p| {
            p.trim_start_matches('-')
                .split('=')
                .next()
                .unwrap_or("")
                .trim()
        })
        .filter(|k| !k.is_empty() && !skill.args.iter().any(|a| a == k))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(format!(
        "unknown argument '{}' for skill '{}' (valid args: {})",
        unknown.join(", "),
        skill.name,
        skill.args.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gray-skills-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    fn write_skill(root: &Path, name: &str, front: &str) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\n{front}\n---\nbody of {name}\n"),
        )
        .unwrap();
    }

    fn skill(name: &str, description: &str) -> Skill {
        Skill {
            name: name.into(),
            description: description.into(),
            path: PathBuf::from("/x/SKILL.md"),
            user_only: false,
            args: vec![],
        }
    }

    #[test]
    fn frontmatter_handles_block_scalars_and_flags() {
        let (fm, rest) = split_front(
            "---\nname: \"a\"\ndescription: >\n  one\n  two\ndisable-model-invocation: true\nargs: [--x, y]\n---\nbody\n",
        )
        .unwrap();
        assert_eq!(fm.name.as_deref(), Some("a"));
        assert_eq!(fm.description.as_deref(), Some("one two"));
        assert!(fm.user_only);
        assert_eq!(fm.args, ["x", "y"]);
        assert_eq!(rest, "body\n");
        assert!(split_front("---\nname: open\n").is_none());
        assert_eq!(body("no front"), "no front");
    }

    #[test]
    fn project_skills_load_and_first_root_wins_a_name() {
        let base = tmp("trust");
        let (gray, proj) = (base.join("gray"), base.join("proj"));
        fs::create_dir_all(proj.join(".git")).unwrap();
        write_skill(&gray.join("skills"), "dup", "description: from user");
        write_skill(
            &proj.join(".gray/skills"),
            "dup",
            "description: from project",
        );
        write_skill(
            &proj.join(".gray/skills"),
            "commit",
            "description: project only",
        );
        write_skill(
            &proj.join(".gray/skills/nested"),
            "deep",
            "description: nested",
        );

        let names = |list: Vec<Skill>| list.into_iter().map(|s| s.name).collect::<Vec<_>>();
        let found = discover_in(&proj, &gray, None);
        assert_eq!(found[2].description, "from user");
        assert_eq!(names(found), ["commit", "deep", "dup"]);
    }

    #[test]
    fn every_name_survives_and_descriptions_share_the_budget() {
        let skills = [
            skill("alpha", &"a".repeat(90)),
            skill("beta", &"b".repeat(90)),
        ];
        let text = listing(&skills, &State::default(), 95).unwrap();
        assert!(
            text.contains(&format!("- alpha: {}…\n", "a".repeat(37))),
            "{text}"
        );
        assert!(
            text.contains(&format!("- beta: {}…\n", "b".repeat(37))),
            "{text}"
        );

        let none = listing(&skills, &State::default(), 0).unwrap();
        assert!(none.contains("- alpha\n") && none.contains("- beta\n"));
    }

    #[test]
    fn descriptions_cap_per_skill() {
        let long = "x".repeat(MAX_DESCRIPTION_CHARS + 50);
        let text = listing(&[skill("a", &long)], &State::default(), 10_000).unwrap();
        assert!(text.contains(&format!("{}…", "x".repeat(MAX_DESCRIPTION_CHARS))));
        assert!(!text.contains(&"x".repeat(MAX_DESCRIPTION_CHARS + 1)));
    }

    #[test]
    fn hidden_skills_stay_out_of_the_listing() {
        let mut user_only = skill("mine", "d");
        user_only.user_only = true;
        let skills = [user_only, skill("off", "d"), skill("on", "d")];
        let mut state = State::default();
        state.disabled.insert("off".into());
        let text = listing(&skills, &state, 1000).unwrap();
        assert!(text.contains("- on: d") && !text.contains("mine") && !text.contains("- off"));
        state.off = true;
        assert!(listing(&skills, &state, 1000).is_none());
    }

    #[test]
    fn budget_is_one_percent_of_the_window() {
        assert_eq!(DEFAULT_CONTEXT_WINDOW / 100 * CHARS_PER_TOKEN, 8000);
    }

    #[test]
    fn args_validate_against_the_declared_set() {
        let mut s = skill("a", "d");
        assert!(validate_args(&s, "anything").is_ok());
        s.args = vec!["dry".into()];
        assert!(validate_args(&s, "--dry=1").is_ok());
        assert!(validate_args(&s, "--wet").unwrap_err().contains("wet"));
    }
}
