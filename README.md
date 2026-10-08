<p align="center">
  <img src="assets/gray-logo.svg" alt="gray" width="96">
</p>
<h1 align="center">gray-skills</h1>
<p align="center">A skill hub — search npm, GitHub, and the skills CLI for agent skills and install them into `~/.gray/skills/`.</p>
<p align="center">
  <a href="https://github.com/vstaln/gray-skills/blob/main/LICENSE"><img alt="MIT License" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <img alt="gray plugin" src="https://img.shields.io/badge/gray-plugin-7aa2f7.svg">
  <img alt="rust" src="https://img.shields.io/badge/built%20with-rust-orange.svg">
</p>

A skill hub for gray: search npm, GitHub, and the skills CLI for agent skills and install them into `~/.gray/skills/`.

## What it does

- `skills_search` tool — probes real sources: npm `keywords:agent-skills`,
  GitHub `path:SKILL.md` via `gh search`, and the `npx skills` ecosystem
  CLI when present. All degrade silently on timeout/offline.
- `skills_install` tool — fetches a pack (npm tarball, git repo w/
  optional sparse subdir, or raw SKILL.md URL), finds every dir with
  SKILL.md frontmatter, and copies it to `~/.gray/skills/<safe-name>/`.
  Reinstalls replace; packs without frontmatter are refused.
- `skills_list` tool and `/skills` command — what's installed, with
  one-line descriptions.

## Wire methods

`plugin/manifest` · `tool/call` (`skills_search`, `skills_install`,
`skills_list`) · `command/run` (`/skills`) · `plugin/shutdown`

## Install

```sh
gray plugin install skills
```

No host capabilities required.

---
Part of the [gray](https://github.com/vstaln/gray) plugin ecosystem —
the open-source AI agent harness. <https://gray.alignment.id>
