# Agent Skills

Rynna supports the [Agent Skills standard](https://agentskills.io/specification): a
local directory containing `SKILL.md` with YAML frontmatter and Markdown
instructions. Existing standard skill packages can be used without converting
them to a Rynna format.

## Add skills to a profile

Place a skill on the machine running Rynna. For example, beside `config.yaml`:

```text
config.yaml
skills/
  code-review/
    SKILL.md
    references/
      checklist.md
```

`skills/code-review/SKILL.md`:

```markdown
---
name: code-review
description: Review code changes for correctness and maintainability. Use when asked to review a patch.
license: MIT
---
Read references/checklist.md using read_skill before reviewing.
Inspect the changed code using the profile's available tools.
Report actionable findings with file locations and explain their impact.
```

`skills/code-review/references/checklist.md`:

```markdown
Check behavior at input boundaries, error paths, and compatibility with callers.
Look for tests that exercise the changed behavior.
```

In **Settings → Profiles**, select a profile, enter `code-review` in **Skills**,
and save. Enter one name or directory per line. Alternatively edit the profile's
existing catalog mapping:

```yaml
profiles:
  work:
    active_skills:
    - code-review
    - ./team-skills/rust
```

Only that profile receives these skills. Empty `active_skills: []` disables all
skills for the profile. New profiles start with none. Renaming preserves the
selection; deleting a profile removes its selection, but never deletes packages.
Restart Rynna after changing the selection or a `SKILL.md` file. This follows the
existing profile catalog lifecycle across CLI, HTTP/web, and desktop.

## Locations and validation

A bare name such as `code-review` is looked up in this order (first match wins):

1. `<catalog directory>/skills/<name>/SKILL.md`
2. `<catalog directory>/.agents/skills/<name>/SKILL.md`
3. `<platform config directory>/rynna/skills/<name>/SKILL.md`
4. `~/.agents/skills/<name>/SKILL.md`

Use `./team-skills/code-review`, an absolute directory, `~/skills/code-review`,
or a path ending in `/SKILL.md` to select a package explicitly. Relative paths
resolve against the directory containing `config.yaml`, including a custom
`--config`/`RYNNA_CONFIG` path. In-memory catalogs use the working directory.
Discovery only resolves selected names; it never enables every installed package.
Browser paths refer to the server's filesystem, not the browser's machine. Mount
packages into containers before enabling them there.

The loader requires valid YAML, a `name` matching the package directory (1–64
lowercase ASCII letters, digits, or hyphens; no leading, trailing or consecutive
hyphens), and a non-empty `description` of at most 1024 characters. Multiline YAML
and optional fields such as `license`, `compatibility`, `metadata` and
`allowed-tools` are preserved in the loaded instructions. Unknown harness fields
are accepted but have no Rynna-specific behavior. Duplicate selected skill names,
missing packages and malformed files produce startup errors identifying the
selection. `rynna profiles` lists configured selections without loading packages
or contacting providers, so the catalog remains inspectable for repair.

## Runtime behavior and permissions

The `read_skill` tool advertises only enabled names and descriptions. The model
loads a relevant skill with `{"name":"code-review"}`, then reads supporting text
with `{"name":"code-review","path":"references/checklist.md"}`. Results include
the absolute package directory for resolving referenced paths. You can explicitly
ask the model to use a skill by name or `$code-review`; activation uses the same
tool (there is no special slash-command parser). Instructions are a startup
snapshot; referenced text resources are read on demand. Empty profiles expose no
skill tool. Both streaming and non-streaming responses use the shared agent tool
loop and its context limits.

Selecting a package authorizes reading its contents, even if it is outside a
native filesystem capability root. The skill reader is read-only, accepts only
relative resource paths, rejects dotfiles, traversal and resource symlinks, and
opens files through the selected directory handle. Reads are limited to 128 KiB
of UTF-8 text per file and 64 selected skills per profile. Binary assets are not
returned by this text tool. A developer-selected package directory may itself be
a symlink; links inside it are rejected.

Skills do not grant additional execution permissions. `allowed-tools` is
informational; scripts require separately configured command capabilities, and
filesystem operations remain subject to the profile's filesystem policy. Skills
cannot enable tools for subscription providers that prohibit external tools.
