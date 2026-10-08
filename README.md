# gray-skills

skill hub browser/installer — search npm/GitHub/skills-cli for agent
skills and install them into `~/.gray/skills/`.

A sidecar plugin for [gray](https://github.com/vstaln/gray), scaffolded by
[gray-account](https://github.com/vstaln/gray-account).

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
