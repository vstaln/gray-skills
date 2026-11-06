<p align="center">
  <img src="assets/gray-logo.svg" alt="gray" width="96">
</p>
<h1 align="center">gray-skills</h1>
<p align="center">Skills for gray — discovery, a budgeted listing, and a hub: search npm, GitHub, and the skills CLI for agent skills and install them into `~/.gray/skills/`.</p>
<p align="center">
  <a href="https://github.com/vstaln/gray-skills/blob/main/LICENSE"><img alt="MIT License" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <img alt="gray plugin" src="https://img.shields.io/badge/gray-plugin-7aa2f7.svg">
  <img alt="rust" src="https://img.shields.io/badge/built%20with-rust-orange.svg">
</p>

Everything gray knows about skills. Bare gray has none; install this to get them.

## What it does

- Discovers skills in `~/.gray/skills`, `~/.claude/skills` (and Claude
  plugins), opencode, `~/.agents/skills`, and the project's `.gray`,
  `.claude`, `.opencode` and `.agents` dirs once `gray trust` covers it.
- Lists them to the model each turn, Claude Code style: every name always
  appears, descriptions share 1% of the context window (at most 1,536
  characters each), and the least-used skills lose theirs first.
  `GRAY_SKILL_CHAR_BUDGET` sets the budget directly.
- `skill` tool — the model loads a skill by name; uses are counted in
  `~/.gray/skills-state.json`.
- `/skills` lists, `/skills <name> [args]` runs one, `/skills on|off`
  toggles the listing, `/skills enable|disable <name>` hides one,
  `/skills remove <name>` deletes one from `~/.gray/skills`.
- `skills_search` tool — probes real sources: npm `keywords:agent-skills`,
  GitHub `path:SKILL.md` via `gh search`, and the `npx skills` ecosystem
  CLI when present. All degrade silently on timeout/offline.
- `skills_install` tool — fetches a pack (npm tarball, git repo w/
  optional sparse subdir, or raw SKILL.md URL), finds every dir with
  SKILL.md frontmatter, and copies it to `~/.gray/skills/<safe-name>/`.
  Reinstalls replace; packs without frontmatter are refused.

## Wire methods

`plugin/manifest` · `prompt/context` · `tool/call` (`skill`,
`skills_search`, `skills_install`) · `command/run` (`/skills`) ·
`plugin/shutdown`

## Install

```sh
gray plugin install skills
gray plugin enable skills --global
```

`--global` keeps the listing on under lean mode, gray's default.

No host capabilities required.

---
Part of the [gray](https://github.com/vstaln/gray) plugin ecosystem —
the open-source AI agent harness. <https://gray.alignment.id>
