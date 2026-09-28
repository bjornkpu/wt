# wt feature inventory

Mined 2026-09-28 from the Python original at `~/.config/wt` (working copy, including uncommitted changes on top of 208c2fe). Line numbers refer to that `wt.py`. This is the reference for the Rust port, not a spec: the spec decides what is kept, fixed or dropped.


I read all of these and changed nothing: `wt.py` (2438 lines), `test_wt.py` (1649 lines), `config.toml`, `shell/wt.ps1`, `README.md`, `.githooks/pre-commit`, `ruff.toml`, `ty.toml`, the shims `~/.local/bin/wt.cmd` and `~/.local/bin/wt`, and the hook wiring in `~/.claude/settings.local.json`.

Two findings up front:
- `wt new fix "msal login loop"`, the example in the module docstring and in the `config.toml` comment, fails. The parser takes a single positional, so it exits 2.
- `wt rm --merged` removes brand-new worktrees that have no commits yet, and `if_merged` then deletes their branches too. Details in section 8.

---

## 0. Entry, shims and dependencies

- **PEP 723 header**: `requires-python = ">=3.11"` and **no dependencies**. Only the standard library is used (`tomllib`, `urllib`, `argparse`, `concurrent.futures`, `difflib`, `json`, ...). The tests use stdlib `unittest`, not pytest.
- **Shims**
  - `wt.cmd`: `uv run --no-project "%USERPROFILE%\.config\wt\wt.py" %*`
  - `wt` (git-bash): `exec uv run --no-project "$(cygpath -w "$HOME/.config/wt/wt.py")" "$@"`
- **Claude Code hooks** live in `~/.claude/settings.local.json`, not `settings.json` as the README says:
  - `WorktreeCreate`: `uv run "$HOME/.config/wt/wt.py" hook-create`, timeout 300
  - `WorktreeRemove`: `uv run "$HOME/.config/wt/wt.py" hook-remove`, timeout 120
  - Neither passes `--no-project`.
- **Checks gate**: `.githooks/pre-commit` runs `uvx ruff check .`, then `uvx ty check --config-file ty.toml .`, then `uv run test_wt.py`. It first unsets `GIT_DIR GIT_INDEX_FILE GIT_WORK_TREE GIT_PREFIX GIT_OBJECT_DIRECTORY`.

## 1. CLI surface

Parser: `build_parser` (2336).

**Global flag**
- `-q/--quiet` is declared on the top-level parser, so it must come **before** the verb: `wt -q new ...`. Written after the verb (`wt new x -q`), argparse rejects it with exit 2.
- It suppresses chatter only. Failures are still printed.

**Shared flag groups**
- "opener" group (new, item, pr, branch):
  - `--open`: store_true.
  - `--run CMD`.
- "naming" group (new, item):
  - `--type`: free string. Help text is `feat|fix|...`, but there is **no `choices`** validation.
  - `--slug`: free string, not validated.
  - `--no-llm`: store_true.
- `main()` (2416) refuses `--run` without `--open`: `die("--run has nowhere to run without --open")`.

**Subcommands**

| verb | args | needs provider | fn |
|---|---|---|---|
| `new` | `spec` (one positional; metavar `<type>/<slug> \| description`) + opener + naming | yes | cmd_new 1464 |
| `item` | `id` + opener + naming | yes | cmd_item 1552 |
| `pr` | `id` + opener | yes | cmd_pr 1577 |
| `branch` | `name`, `--track` + opener | yes | cmd_branch 1602 |
| `rm` | `name?`, `--stale`, `--merged`, `--force`, `--dry-run` | no | cmd_rm 2088 |
| `ls` | none | no | cmd_ls 2215 |
| `cd` | `name?` | no | cmd_cd 2156 |
| `open` | `name` (required), its own `--run CMD`; sets `open=True` by default | no | cmd_open 2168 |
| `complete` | none | no | cmd_complete 2195 |
| `hook-create` | `--name` | yes | cmd_hook_create 2287 |
| `hook-remove` | none | no | cmd_hook_remove 2317 |

A verb is required (`required=True`).

**main() flow**
1. Parse args.
2. `root = main_root()`.
3. `cfg = load_config(root)`.
4. `provider = provider_of(root, cfg)` if the verb needs one, else `Provider()`.
5. `result = fn(args, Ctx)`.
6. If the result is a `Path`, **print it to stdout**. This is the only place stdout gets a path.

**Exit codes**
- 0: success.
- 1: a `WtError`. Printed to stderr as `wt: {msg}`.
- 2: an argparse error. Usage goes to stderr.
- Anything other than `WtError` gives a Python traceback and exit 1. Examples: the `OSError` re-raised from `seed_files` after rollback, a `ValueError` from `fromisoformat` on a malformed `created`, an `AttributeError` from a sidecar that is valid JSON but not an object, an exception from `llm_name` coming through the Future.

**stdout vs stderr**
- stdout carries only: the worktree path (new, item, pr, branch, cd, hook-create), the `ls` table, and the `complete` names.
- Everything else goes to stderr through `log()` (208). That includes `exec` step output, which is re-written to stderr.

### new (cmd_new 1464)
- `conf = resolve(cfg, "new", provider.name)`.
- If `spec` matches `BRANCH_SPEC` (`^[a-z0-9]+/[a-z0-9]+(?:[-/][a-z0-9]+)*$`), it is used verbatim as the branch. A fetch runs if `conf.fetch` is set, and no model is called.
- Otherwise `choose_name(text, fallback_type="feat")` runs, and `branch = fill(conf.branch or "{type}/{slug}", type, slug)`.
  - Only `{type}` and `{slug}` are supplied here. A template that uses `{id}` dies.
- `base = resolve_base(...)`.
- `CreateSpec(mode="new", branch, base)`, then `create(..., defer_exec=args.open)`.
- With `--open`: `open_workspace(cfg, "new", ..., cmds, branch=branch)`. Its detail is logged as `  {detail}`. **A failed open does not change the exit code.**

### item (cmd_item 1552)
- `conf = resolve(cfg, "item", provider)`.
- `name_for_item` (1514):
  - If a sidecar already has `id == <id>` and a branch, that branch is reused. It logs `  {id}: reusing {branch} from an earlier run`, and the tracker lookup and the model are both skipped.
  - Otherwise it calls `provider.lookup_item` and logs `  {id}: {title}  [{type}]`.
  - Then `choose_name(title, fallback_type=type_map.get(type, "feat"))`.
  - If naming fell back, `push` is on (default True) and `--no-llm` was not given, it dies with: `naming fell back to the mechanical slug ({typ}/{slug}): {reason}. That name would be pushed and linked. Re-run with --no-llm to accept it, or pass --slug.`
  - `branch = fill(conf.branch or "{type}/{id}-{slug}", type, id, slug)`.
- `CreateSpec(mode="item", item_id=id, meta={"title": title})`. `title` is `""` on the reuse path.
- `create`.
- `publish_item` (1486):
  - If `conf.push` (default **True**): `git push -u <remote> <branch>` in the worktree.
    - On failure it records `push failed: {stderr[:400]}` and `skipped link/state: branch is not on origin`, and returns.
    - On success it records `pushed {branch}`.
  - If `conf.link_branch` (default **False**): `provider.link_branch`.
  - If `conf.set_state` is truthy: `provider.set_state(id, state)`.
  - These are two separate PATCHes on purpose.
- With `--open`: the open outcome is **recorded** in the Results.
- `Results.report(quiet)`:
  - Notes print as `  {note}` unless quiet.
  - Failures always print, as `  {failure}`.
- If there are any failures, it dies with `{n} post-create step(s) failed; the worktree is at {path}`. The path is then not printed to stdout.

### pr (cmd_pr 1577)
- Fetch if `conf.fetch`.
- `provider.lookup_pr`.
- `dirname = fill(conf.dirname or "review/pr-{id}", id)`.
- Logs `  PR {id}: {branch} @ {sha[:8]}`.
- `CreateSpec(mode="pr", base=sha, detach=True, dirname, item_id=id, meta={"pr_branch": branch})`.
  - `detach` is hard-coded. `conf.detach` is ignored.
- `create`, plus the optional open (label args `id` and `branch=pr_branch`). The open outcome is only logged.

### branch (cmd_branch 1602)
- Fetch if `conf.fetch`.
- `detach = not args.track and conf.detach (default True)`.
- Detached:
  - `base = "{remote}/{name}"`
  - `dirname = fill(conf.dirname or "review/{slug}", slug=name)`
  - `branch = None`
- `--track` (or `detach=false` in config):
  - `base = name`
  - `dirname = name` (not filled)
  - `branch = name`
  - `track = args.track`
- The open label gets `branch=name`.

### rm (cmd_rm 2088)
- `trees` = all worktrees except the one whose path equals root.
- If `[teardown] fetch` is set (the top level only; mode-level `fetch` is ignored), it fetches first. This happens **before** the argument checks.
- `--stale` together with `--merged`: `die("--stale and --merged select different things; run them separately")`.
- `--stale`: `rm_sweep(stale_targets)`.
- `--merged`: `rm_sweep(merged_targets)`.
- No name and no sweep flag: `die("give a name/branch, or --stale / --merged")`.
- A name: `remove_one(select_one(trees, name), force, quiet, dry_run)`.
- `rm_sweep` (2057), messages on stderr:
  - `skipped {path.name}: {why}` for each skipped tree.
  - If nothing is due: `nothing to remove`, exit 0.
  - For each due tree: `{why}: {path}`. A `WtError` is caught and logged as `  {e}`.
  - Afterwards: `  left in place: {path}` for each tree that failed.
  - Then `die("{n} of {m} worktrees could not be removed")`.

### ls (cmd_ls 2215)
- One row per worktree, in git's order.
  - The main worktree: `Row("(main)", branch or "-", "", "", "")`.
  - The others: `leaf_of`; ref is `branch` or `detached {head[:8]}`; mode is `meta.mode` or `""`; age is `{days}d` from `meta.created`; flags are `dirty` (from `git status --porcelain`, which **fails open**: an error means not dirty) and `exec-failed` (sidecar has `exec_failed`), joined by spaces.
- Output: each column `ljust` to its max width, columns joined by two spaces, then `rstrip`. Printed to stdout.
- No rows (a bare repo): prints nothing.

### cd (cmd_cd 2156)
- No name: returns root, so the main worktree's path is printed.
- Otherwise returns `select_one(trees, name).path`.

### open (cmd_open 2168)
- `select_one`, then reads the sidecar.
- `open_workspace(cfg, meta.mode or "new", root, path, [run] if run, branch=w.branch or "", id=meta.id or "")`.
- `log(out.detail)`.
- **Nothing goes to stdout, and it exits 0 even when `ok` is False.**

### complete (cmd_complete 2195)
- A sorted set of `leaf_of(...)` for each non-main worktree, plus each branch name.
- One per line on stdout.

### hook-create (cmd_hook_create 2287)
- Reads the payload from stdin (`read_hook_payload` 2249):
  - Decoded as utf-8 with `replace`.
  - Empty input gives `{}`.
  - Invalid JSON: `die("hook payload was not JSON")`.
  - Not a dict: `die("hook payload was a JSON {type}, expected an object")`.
- Name (`hook_branch_name` 2271): `payload.name`, else `--name`, else `claude-{(session_id or sessionId)[:8]}`, else `claude-{pid}`.
- `conf = resolve(cfg, "new", ...)`. Fetch if `conf.fetch`.
- Branch: the name if it matches `BRANCH_SPEC`, else `{conf.hook_type or "feat"}/{slugify(name, words=6, stopwords)}`.
- `create(CreateSpec(mode="new", meta={"via": "claude"}), quiet=True)`.
  - `exec` runs here, not deferred. Its output goes to stderr.
- The path goes to stdout through `main`.

### hook-remove (cmd_hook_remove 2317)
- Target is `payload.worktreePath` or `payload.path`. Missing: `die("hook-remove: payload carried no worktreePath")`.
- Match is an exact `str(w.path) == str(target)`, skipping the main tree.
- On a match: `force = resolve_teardown(cfg, "hook").force (default False)`, then `remove_one(quiet=True)`, then logs `hook-remove: removed {path}`.
- No match: `die("hook-remove: no worktree at {target!r}")`.

## 2. Config

**Loading** (`load_config` 455)
- `DEFAULTS` (97), then deep-merged with `~/.config/wt/config.toml` (`Path.home()`), then with `<main_root>/.wt.toml`.
- Each file is validated on its own before merging. Messages name the file path.
- Tables merge; **arrays and scalars replace** (`deep_merge` 323).
- `.wt.toml` is read from the **main** worktree root only.

**Option resolution**
- Create options (`resolve` 466): `[defaults]`, then the scalars of `[mode.<mode>]` (nested dicts skipped), then `[mode.<mode>.<provider>]`. Provider is `ado`, `github` or `none`.
- Teardown (`resolve_teardown` 481): the scalars of `[teardown]` (excluding `mode`), then `[teardown.mode.<mode>]`.

**DEFAULTS (layer zero)**
```
defaults: root="../{repo}.worktrees", fetch=True, base="origin/{default_branch}", branch="{type}/{id}-{slug}",
          copy=[], symlink=[], exec=[], exec_strict=False, exec_timeout=600
mode.new:    branch="{type}/{slug}"
mode.branch: detach=True, dirname="review/{slug}", copy=[]
mode.pr:     dirname="review/pr-{id}", detach=True, copy=[]
teardown: require_clean=True, delete_branch="if_merged", purge_copied=True, prune=True, fetch=False, mode.hook.force=False
naming: llm=False, model="haiku", timeout=60
```

**Validation** (`validate` 440; the whole list is reported in one message: `config problems:\n  ` joined with `\n  `)

`OPTION_TYPES` (336). Checked in `[defaults]`, `[mode.X]` and `[mode.X.<sub>]`:

| key | type |
|---|---|
| root | str |
| remote | str |
| default_branch | str |
| fetch | bool |
| base | str |
| branch | str |
| dirname | str |
| detach | bool |
| track | bool |
| copy | list |
| symlink | list |
| exec | list |
| exec_strict | bool |
| exec_timeout | int |
| shell | list |
| push | bool |
| link_branch | bool |
| set_state | str |
| hook_type | str |

`TEARDOWN_TYPES` (357): fetch bool, require_clean bool, delete_branch str, purge_copied bool, prune bool, force bool, ttl_days int.

`NAMING_TYPES` (366): llm bool, model str, timeout int, system str, stopwords list.

Rules:
- Every value that is a dict is skipped by `_check_table`.
- An unknown key produces `{where} [table]: unknown key 'k' (did you mean X?)`, using `difflib.get_close_matches`.
- A wrong type produces `... k should be int, got str`. A bool does not count as an int.
- Modes must be one of `MODES = ("new","item","pr","branch","hook")`:
  - `[mode.x]`: `... [mode.x]: unknown mode (one of new, item, pr, branch, hook)`
  - `[teardown.mode.x]`: `... [teardown.mode.x]: unknown mode`
- `delete_branch` must be in `("never","if_merged","always")`. Otherwise: `{where} {scope}: delete_branch='x' is not one of never, if_merged, always - a value wt does not recognise would delete the branch, not keep it`.
- Each `[naming.type_from_tracker]` value must be in `TYPES = feat fix chore docs refactor test perf build ci`. Otherwise: `... [naming.type_from_tracker]: 'k' maps to 'v', not one of ...`.
- **Not validated**: the `[herdr]` table, unknown top-level tables, top-level scalars, and the key names inside `type_from_tracker`.

**Keys read, and where**

| key | read in | default in code |
|---|---|---|
| defaults.remote | remote_of 315 | "origin" |
| defaults.default_branch | default_branch 278 (fallback) | "main" |
| root (resolved per mode) | wt_root_of 493 | "../{repo}.worktrees"; placeholder `{repo}` = root.name |
| fetch | cmd_new, choose_name, cmd_pr, cmd_branch, hook-create | True |
| base | resolve_base 1456 | "origin/{default_branch}"; placeholders `{default_branch}`, `{remote}` |
| branch | new: `{type}{slug}`; item: `{type}{id}{slug}` | see DEFAULTS |
| dirname | pr (`{id}`), branch (`{slug}`) | see DEFAULTS |
| detach | branch mode only | True |
| copy / symlink | seed_files 1214 | [] |
| exec | exec_cmds 1250; placeholders `{branch}`, `{id}`, `{path}` | [] |
| exec_strict | create | False |
| exec_timeout | run_exec | 600 |
| shell | exec_shell 1237 | platform default |
| push | publish_item, name_for_item | True |
| link_branch | publish_item | False |
| set_state | publish_item | unset |
| hook_type | hook-create, read from the **mode.new** conf | "feat" |
| teardown.fetch | cmd_rm, top level only | False |
| require_clean / purge_copied / prune | remove_one | True |
| delete_branch | plan_removal | "if_merged" |
| force | hook-remove only, `teardown.mode.hook.force` | False |
| ttl_days | stale_targets | none (the tree is skipped) |
| naming.llm / model / timeout / system | llm_name | False / "haiku" / 60 / NAMING_SYSTEM |
| naming.stopwords | naming_stopwords 1131 | STOPWORDS (1005) |
| naming.type_from_tracker | provider_of, merged over TYPE_FROM_TRACKER (151) | bug→fix, task→chore, user story / product backlog item / feature / epic→feat |
| herdr.label.<mode> / herdr.label_default | label_for 920; placeholders `{repo}`, `{id}`, `{branch}` (all default to "") | "{repo}" |

**What the user's `config.toml` adds on top of DEFAULTS**
- `remote = "origin"`, `copy = [".env", ".databricks"]`, `exec_timeout = 600`, `exec_strict = false`.
- `[mode.item]`: `push = true`, `link_branch = true`, `set_state = "Active"`.
- `[mode.item.github]`: `push = true`, `link_branch = false`, `set_state = ""`.
- `[mode.pr]`: `copy = []`, `exec = []`. `[mode.branch]`: `detach = true`, `copy = []`, `exec = []`.
- herdr labels: `label_default = "{repo} · {branch}"`, `item = "{repo} · #{id}"`, `pr = "{repo} · PR {id}"`, `new = "{repo} · {branch}"`, `branch = "{repo} · review {branch}"`.
- `[teardown.mode.pr]`: `require_clean = false`, `delete_branch = "never"`, `ttl_days = 3`.
- `[teardown.mode.branch]`: `delete_branch = "never"`.
- `[naming]`: `llm = true`, `model = "haiku"`, `timeout = 60`, plus `type_from_tracker` (the same as the built-ins).

**Environment variables**
- Read:
  - `HERDR_PANE_ID` (release_panes 1852).
  - Home through `Path.home()` (`USERPROFILE` on Windows).
  - `os.environ` is inherited by every child process.
- Set for `az`, `gh`, `herdr` and `claude` (`cli_env` 767): `PYTHONIOENCODING=utf-8`, `PYTHONUTF8=1`, `AZURE_CORE_ONLY_SHOW_ERRORS=true`, `AZURE_CORE_COLLECT_TELEMETRY=false`, `AZURE_EXTENSION_USE_DYNAMIC_INSTALL=no`.
- git and the exec steps inherit the plain environment.

**Template `fill`** (1146)
- Sequential `str.replace` of `{k}`.
- If any `{` remains afterwards: `die("unresolved placeholder in template {t!r} -> {out!r}")`.
- This also fires on a literal `{` in an exec command, or in a substituted value.

## 3. External processes and filesystem side effects

### git
Called through `git_try` (174): `["git", *args]`, capture, `encoding=utf-8, errors=replace`, stdin set to DEVNULL, timeout 120s by default. A timeout gives `die("git {args} timed out after {t}s")`. `git()` (201) dies on a non-zero exit with `git {args}\n{stderr or stdout}` and returns stripped stdout.

| call | where |
|---|---|
| `rev-parse --git-common-dir` (relative result made absolute against the cwd, then resolved) | common_dir 225. main_root = the parent of the common dir if it is named `.git`, else the common dir itself. meta_dir = `<common>/wt` |
| `symbolic-ref --quiet refs/remotes/<remote>/HEAD`, then `ls-remote --symref <remote> HEAD` (20s timeout). The default branch is the last `/` segment. If both fail it logs `wt: could not read the remote's default branch; assuming {fallback}` | _default_branch 256, memoized per (root, remote, fallback) |
| `worktree list --porcelain` | worktrees 319 |
| `remote get-url <remote>` (check=False) | provider_of |
| `fetch --prune <remote>`. A failure logs `wt: fetch failed, working from possibly stale refs: {stderr[:400]}` and the run continues | fetch 1406 |
| `worktree add --detach <path> <base>` / `rev-parse --verify --quiet <branch>`, then `worktree add <path> <branch>` / `worktree add --guess-remote <path> <branch>` (track) / `worktree add -b <branch> <path> <base>` | add_worktree 1191. Returns made_branch: True only for the last two |
| `worktree remove --force <path>`, `branch -D <branch>` (only if made_branch) | roll_back 1326 |
| `push -u <remote> <branch>` (cwd = the worktree) | publish_item |
| `status --porcelain`, `rev-parse --abbrev-ref HEAD`, `rev-parse --abbrev-ref --symbolic-full-name @{u}`, `rev-list --count <rng>` | dirty_reasons 1682 |
| `for-each-ref --format=%(refname:short) %(upstream:short) %(upstream:track) refs/heads` | gone_upstreams 1996. Parses lines whose track is `[gone]` |
| `branch --format=%(refname:short) --merged <remote>/<default>` | merged_targets 2024 |
| `branch -d` or `-D <branch>`. A failure logs `  kept branch {b}: {stderr}` unless quiet | delete_branch 1745 |
| `worktree remove [--force] <path>`, retried | remove_checkout 1876 |
| `worktree prune` (check=False) | remove_one, if `prune` |
| `status --porcelain` (check=False) | cmd_ls |

### az
Called through `run_cli` (717):
- `shutil.which(argv[0])`. Missing: `die("{x} not found on PATH")`.
- On Windows, a `.cmd` or `.bat` runs through `shell=True` with `list2cmdline`.
- Capture as bytes, then `decode()` tries utf-8, then cp1252, then latin-1.
- stdin DEVNULL, `env=cli_env()`, timeout 60s by default. A timeout gives `die("{x} timed out after {t}s")`.

`run_json` (752):
- rc != 0: `die("{x} failed\n{err or out}")`.
- Output is not JSON: `die("{x} returned non-JSON:\n{out[:400]}")`.
- Output is JSON but not an object: `die("{x} returned a JSON {type}, expected an object")`.

The az calls. `_argv` appends `--only-show-errors`, then `--org <ado_account>` when an org is known:
- `az repos pr show --id <id> -o json`
  - Reads `lastMergeSourceCommit.commitId` and `sourceRefName`, with `refs/heads/` stripped.
  - No sha: `die("PR {id}: Azure DevOps has not computed the merge commit yet; retry shortly")`.
- `az boards work-item show --id <id> --fields System.Title,System.WorkItemType --expand none -o json`
  - Returns `{title, type: lower()}`.
- `az repos show --repository <repo> --project <project> -o json`
  - Reads `id` and `project.id`. If either is missing, the outcome is `link_branch: az repos show returned no repo/project id` (a failure).
- `az account get-access-token --resource 499b84ac-1321-427f-aa17-267ca6975798 --query accessToken -o tsv --only-show-errors`
  - Cached per root on success. A failure clears the cache (ado_token 792 / _fetch_ado_token 806).

### Direct HTTP to ADO (ado_request 850)
- Skipped with detail `could not parse org/project from origin` if org or project is missing.
- No token: failure `could not get an ADO token: {why}`.
- Request:
  - `PATCH {account}/{quote(project)}/_apis/wit/workitems/{quote(id)}?api-version=7.1`
  - Headers `Authorization: Bearer <tok>`, `Content-Type: application/json-patch+json`
  - Body is the JSON patch. Timeout 15s.
- Redirects are refused (`_NoRedirect` 827). The reason text is `refusing redirect to {newurl}`.
- Retry: 3 attempts, sleeping `0.5 * 2**attempt * (1 + random())`. Only transport errors, 429 and >=500 are retried.
- Result:
  - An `HTTPError` gives the detail `HTTP {code} {reason}: {body[:400]}`.
  - Transport errors give `{ExcType}: {e}`.
  - A 200 with a json content type is ok. A non-200 gives `HTTP {status}: {body}`. A non-json type gives `expected JSON, got {ctype}`.
- `ado_account` (535): the base itself if it ends in `.visualstudio.com`, else `{base}/{quote(org)}`. The default base is `https://dev.azure.com`.

Patches:
- link_branch:
  - `[{"op":"add","path":"/relations/-","value":{"rel":"ArtifactLink","url":"vstfs:///Git/Ref/{projId}%2F{repoId}%2FGB{quote(branch,safe='')}","attributes":{"name":"Branch"}}}]`
  - Outcomes: `linked branch {b} to {id}`, or `branch {b} was already linked to {id}` (when `already_linked`: `typeKey == RelationAlreadyExistsException`, or a substring match on `RelationAlreadyExists` for a body that is not JSON), or `link_branch failed: {detail[:400]}`, or skipped `link_branch: {detail}`.
- set_state:
  - `[{"op":"add","path":"/fields/System.State","value":state}]`
  - Outcomes: `{id} -> {state}`, `set_state failed: ...`, or skipped `set_state: ...`.

### gh
- `gh pr view <id> --json headRefOid,headRefName`. No sha: `die("PR {id}: gh returned no head commit")`.
- `gh issue view <id> --json title,labels`. The type is the first lowercased label name found in `type_map` (a set is iterated), else `"feature"`.
- GitHub has no link or state support: the base `Provider` gives the skipped outcomes `link_branch: no github equivalent, skipped` and `set_state: no github equivalent, skipped`.
- A provider of `none`:
  - `lookup_pr` dies with `no supported provider on origin; cannot resolve a PR id`.
  - `lookup_item` dies with `...cannot resolve a work item id`.

### claude (llm_name 1073)
- `[which("claude"), "-p", "Work item title: {title}", "--model", model, "--output-format", "text", "--system-prompt", system]`
- Run with `subprocess.run` directly, not through run_cli. `cwd = ~/.config/wt`, so the repo's CLAUDE.md is not loaded. stdin DEVNULL, `env=cli_env()`, timeout `naming.timeout`.
- Output handling: strip code fences, regex `[{].*[}]` (DOTALL), `json.loads`, lowercase type and slug.
- Validation: `type in TYPES` and slug fullmatches `[a-z0-9]+(-[a-z0-9]+){1,5}` (2 to 6 words).
- Reasons returned on failure:
  - `naming model not requested` (llm off, or empty title)
  - `claude is not on PATH`
  - `the naming model timed out after {t}s`
  - `the naming model exited {rc}`
  - `the naming model returned no JSON`
  - `the naming model returned malformed JSON`
  - `the naming model proposed an unusable name ({typ}/{slug})`
- `NAMING_SYSTEM` (1065): `Output ONLY compact JSON: {"type":"...","slug":"..."}. type must be one of: feat fix chore docs refactor test perf build ci. slug must be 2-4 lowercase ENGLISH kebab-case words naming the concrete subject (translate from Norwegian if needed; drop filler words). No prose, no code fence.`
  - It says 2-4 words while the validator accepts 2-6.
- `choose_name` (1424) runs the model in a 1-thread pool **in parallel with the fetch**. The model is skipped when `--no-llm` is given, or when both `--type` and `--slug` are.
  - Precedence: `--type` / `--slug`, then the model, then `fallback_type` / `slugify`.
  - `fallback_reason` is set only if the model was wanted, returned None, and `naming.llm` is true.
  - Unless quiet it logs `  (naming fell back to the mechanical slug: {reason})`.

### herdr
Every call uses a 10s timeout.
- `herdr worktree list --cwd <root>`
  - Reads `result.worktrees[]`. Picks the first tree whose path is the same as or under root and that has `open_workspace_id` (parent_workspace 946).
- `herdr worktree open --workspace <src> --path <path> --no-focus [--label <label>]`
  - The reply's `result.root_pane.pane_id` is read by pane_of 938.
- `herdr pane run <pane> <cmd>`, once per exec or `--run` command, in order.
- `herdr_open` (969) outcomes:
  - Skipped: `herdr not on PATH, skipped`.
  - Failure: `herdr: no workspace open on {root}; open one there first`.
  - Failure: `herdr failed: {err[:400]}`.
  - Ok: `herdr: {label or path}`.
  - Failure: `{opened}, but the reply named no pane to run {n} command(s) in`.
  - Failure: `{opened}, but sending {c!r} failed: ...`.
  - Ok: `{opened}, running {n} command(s) there`.
- `herdr pane list` (`result.panes[]` with pane_id, cwd, agent), then `herdr pane run <pane> cd "<root>"` for each pane under the tree (release_panes 1831).
  - Skipped: the pane whose id equals `HERDR_PANE_ID`, reported as `{pid} (the shell wt is running in)`.
  - Skipped: panes that have an `agent`, reported as `{pid} (running {agent})`.
  - The remaining panes are sent the `cd` in sorted order. The return codes are ignored.

### exec steps (run_exec 1263)
- Per command: `[*exec_shell(conf), cmd]` with cwd = the worktree, text utf-8/replace, stdin DEVNULL, timeout `exec_timeout`. `exec_shell`:
  - `conf.shell` if set.
  - Otherwise POSIX gets `["bash","-lc"]`.
  - Windows gets `["pwsh" if which else "powershell", "-NoProfile", "-Command"]`.
- Unless quiet it logs `  exec: {cmd}` first.
- The command's stdout and stderr are both written to **stderr**.
- A timeout logs `wt: exec timed out after {t}s: {cmd}`. A non-zero exit logs `wt: exec failed (rc={rc}): {cmd}`.
- The function returns the list of failed commands.

### Filesystem side effects

**create** (1341)
1. `conf = resolve(mode, provider)`.
2. `path = (wt_root / (dirname or branch)).resolve()`. If the path is not inside wt_root: `die("refusing to create outside {root}: {path}")`.
3. If the path exists, `reuse_existing`:
   - Detached, and the sidecar base differs from `spec.base`: die with `{path} is pinned to {old[:8]} but {mode} now resolves to {new[:8]}; remove it and re-create`.
   - Otherwise it logs `exists: {path}` and returns.
4. If any worktree already has this branch: `die("branch {b} is already checked out at {p}")`.
5. `mkdir -p` the parent directory. `add_worktree`.
6. `seed_files` (1214):
   - For each rel in `copy`: skipped if the source is missing. Otherwise `mkdir -p` the destination's parent, then `copytree(dirs_exist_ok)` for a directory or `copy2` for a file. Recorded in `copied`.
   - For each rel in `symlink`: `symlink_to(src, target_is_directory)` if the source exists and the destination does not. Symlinks are not recorded.
   - The rel paths are **not** checked to stay inside the tree.
7. Write the sidecar `<git-common-dir>/wt/{urllib.quote(leaf, safe='')}.json` (for example `feat%2Fx.json`), creating the directory, with `indent=2`:
   - `{mode, path (str, resolved), branch|null, base, detached, copied[], created: datetime.now(UTC).isoformat(), **spec.meta}`
   - Plus `id` when there is an item_id. Item and PR both set it.
   - spec.meta is `title` (item), `pr_branch` (pr) or `via: "claude"` (hook).
   - Any exception in seed or meta causes `roll_back` and a re-raise.
8. If `defer_exec` and `exec_strict` and `exec` is non-empty: log `wt: exec runs in the new workspace, so exec_strict cannot roll this tree back`.
9. `run_exec` (unless deferred). On failures:
   - `meta.exec_failed = [...]`, the sidecar is rewritten, and `  {n} exec step(s) failed; the worktree is at {path}` is logged.
   - If `exec_strict`: `roll_back`, then `die("exec failed in {path}: {first} (output above; worktree rolled back)")`.
10. Unless quiet: `created: {path}`.

**roll_back**: logs `wt: rolling back {path}`, runs `worktree remove --force`, runs `branch -D` if made_branch, and unlinks the sidecar.

**remove_one** (1919)
1. `plan_removal` (1768):
   - If `path == main_root`: `die("refusing to remove the main worktree")`.
   - Find the sidecar by matching `meta.path == str(path)` across every `*.json` (sorted; unreadable ones skipped).
   - No sidecar and not force: `die("{path}: no readable wt metadata. Re-run with --force to remove it under the default policy.")`.
   - `td = resolve_teardown(meta.mode or "new")`.
   - `reasons = dirty_reasons`.
   - `branch = meta.branch or wt.branch`.
   - `policy = td.delete_branch or "if_merged"`.
   - `purge` = the copied rels that resolve inside the path.
2. `--dry-run`: `describe_removal` prints `would remove: {path}`, then `  mode:   {mode|(unknown)}`, `  branch: {branch|(detached)} -> {policy}`, `  purge:  {list|(nothing)}`, and `  NOT CLEAN: {reasons}` if there are any. Then it returns.
3. If `require_clean` and not force and there are reasons: `die("{path}: {reasons joined ', '} (use --force to override)")`.
4. If `purge_copied`: `purge_copied` (1730). rmtree for real directories, unlink for files and symlinks. An OSError gives `die("could not purge {t}: {e}")`.
5. `remove_checkout`:
   - `release_panes`, then `git worktree remove [--force]`.
   - Retried while still registered, up to 4 attempts in total, with a 0.4s sleep between them.
   - Success is judged by registration (`registered` 1869 via `same_or_under`), not by the exit code.
   - Still registered: `die("could not remove {path}: {first.stderr[:400]}{; herdr last had X, Y in it}")`.
6. `delete_branch` policy: `never` skips it, `always` uses `-D`, anything else uses `-d`.
7. Unlink the sidecar.
8. `worktree prune`, if `prune`.
9. Unless quiet: `removed: {path}`.
10. `drop_empty_dir`: `rmdir` if the directory still exists. An OSError logs `wt: the empty directory is still held, remove it later: {path} ({strerror})`.

**dirty_reasons** (1682). Every probe fails closed. The possible reasons:
- `could not read status: ...`
- `uncommitted changes`
- `could not read HEAD`
- For a non-detached HEAD, with `upstream = @{u}` or empty and `base = {remote}/{default}`:
  - The ahead count runs only if there is an upstream, or `gone_upstreams().get(branch, base) == base`. In other words, a branch whose upstream is gone (and is not base) skips the count.
  - The range is `{upstream}..HEAD` or `{base}..HEAD`.
  - Possible reasons: `could not compare against {lhs}`, `unpushed commits`.
- **There is no stash probe, on purpose.**

## 4. Shell integration (`shell/wt.ps1`, PowerShell only, dot-sourced from $PROFILE)

The protocol is: **the last line of stdout is a path.**
- The wrapper calls `$HOME\.local\bin\wt.cmd @args` and captures **only stdout**. stderr streams live.
- The captured output is re-emitted with `Write-Output`.
- If the exit code is 0 and the last stdout line is an existing directory (`Test-Path -LiteralPath -PathType Container`), it runs `Set-Location` to that directory.
  - If `--open` is anywhere in the args, it goes to `Get-WtMainRoot` instead, falling back to the printed path when there is no main root.
- A non-zero exit is propagated through `$global:LASTEXITCODE`.
- There are no temp files and no sentinel lines.
- On the wt.py side this means stdout must carry nothing but the path for new, item, pr, branch, cd and hook-create; `ls` and `complete` output; and nothing at all for `open` and `rm`.

**The `rm` pre-step**: before running `rm`, the wrapper does `Set-Location` to the main root whenever the current directory is not already the main root, because Windows will not delete a directory that is a process's cwd.

**Pure-filesystem helpers**
- `Get-WtRepoKey`: the nearest ancestor that contains `.git` (a directory or a file).
- `Get-WtMainRoot`: if `.git` is a directory, that directory's parent folder. Otherwise it parses the first line of the `.git` file, `^gitdir:\s*(.+)$`, walks up to the segment named `.git`, and takes its parent.

**Completion cache**
- A per-repo-key cache of `wt.cmd complete 2>$null` output, with a 60s TTL.
- Invalidated when the verb is one of `new item pr branch rm`.

**Completer** (`Register-ArgumentCompleter -CommandName wt`)
- Verbs: `new item pr branch ls cd rm open`. `complete` and `hook-*` are hidden.
- Flags per verb:
  - new, item: `--open --run --type --slug --no-llm`
  - pr: `--open --run`
  - branch: `--track --open --run`
  - rm: `--stale --merged --force --dry-run`
  - open: `--run`
  - `-q` is not offered.
- Worktree names are offered for `rm`, `cd` and `open`.

There is no bash or zsh wrapper, deliberately. The `~/.local/bin/wt` shim does no cd.

## 5. Pure logic vs IO (line numbers)

**Pure** (or pure apart from `Path.resolve`)
- `Outcome` 54, `Results.record` 79
- `deep_merge` 323, `_check_table` 377, `_check_modes` 398, `_check_teardown` 409, `_check_naming` 430, `validate` 440 (raises)
- `resolve` 466, `resolve_teardown` 481, `wt_root_of` 493 (resolve), `remote_of` 315
- `parse_porcelain` 288 (resolve)
- `ado_account` 535, `AdoProvider._argv` 552, the vstfs artifact string at 624, the ADO URL build at 863-868
- `_retryable` 844, `already_linked` 912
- `label_for` 920, `pane_of` 938
- `naming_stopwords` 1131, `slugify` 1137, `fill` 1146, `BRANCH_SPEC` 1063
- `CreateSpec.leaf` 1176, `exec_cmds` 1250, `open_cmds` 1256
- `decode` 780, `same_or_under` 1805, `find_targets` 1956, `select_one` 2142
- `hook_branch_name` 2271 (uses getpid only as a fallback)
- `leaf_of` 2129 (resolve), `describe_removal` 1794 (formatting only, then log)

**Pure fragments inside IO functions, worth extracting for the Rust core**
- URL to provider/org/project/repo parsing (689-714), plus the `[A-Za-z0-9._ -]+` safety check (708).
- Parsing of the claude output (1116-1128).
- `gone_upstreams` line parsing (2016-2021).
- The merged/gone classification (2046-2053).
- The TTL decision (1978-1992; needs `now` injected).
- The decision inside `dirty_reasons`: which range to count (1712-1719).
- The `add_worktree` arm selection (1197-1211).
- The worktree path plus containment check (1183).
- The sidecar payload build (1362-1373) and filename rule (1631).
- `plan_removal` policy (1782-1791).
- `ls` row and width formatting (2233-2246).
- The GitHub label to type choice (667-670).
- The ADO response to Outcome mapping (637-652).
- `choose_name` precedence (1447-1450).
- `name_for_item`'s die condition (1539).
- Branch derivation for `hook-create` (2297).
- `cmd_branch` detach, base and dirname derivation (1607-1609).

**IO**
- `git_try` 174, `git` 201, `common_dir`, `main_root`, `meta_dir`, `ensure_meta_dir` 225-252
- `_default_branch` / `default_branch` 256/278, `worktrees` 319, `load_config` 455, `provider_of` 683
- `run_cli` 717, `run_json` 752, `cli_env` 767 (reads env)
- `ado_token` / `_fetch_ado_token` 792/806, `ado_request` 850
- All `Provider` lookups and writes 517-671
- `parent_workspace` 946, `herdr_open` 969, `open_workspace` 931, `llm_name` 1073
- `worktree_path` 1183 (resolve), `add_worktree` 1191, `seed_files` 1214, `exec_shell` 1237 (which)
- `run_exec` 1263, `write_meta` 1304, `reuse_existing` 1309, `roll_back` 1326, `create` 1341
- `fetch` 1406, `choose_name` 1424 (threads), `resolve_base` 1456
- `publish_item` 1486, `name_for_item` 1514
- `iter_meta` 1634, `meta_for_path` 1647, `branch_for_item` 1661, `meta_of` 1676
- `dirty_reasons` 1682, `purge_copied` 1730, `delete_branch` 1745, `plan_removal` 1768
- `panes_now` 1820, `release_panes` 1831, `registered` 1869, `remove_checkout` 1876, `drop_empty_dir` 1907, `remove_one` 1919
- `stale_targets` 1974, `gone_upstreams` 1996, `merged_targets` 2024, `rm_sweep` 2057
- All `cmd_*`, `read_hook_payload` 2249, `main` 2416, `log` 208

## 6. Interactive parts and platform branches

- **Nothing is interactive**: no fzf, no pickers, no prompts. Every child process gets stdin=DEVNULL so a credential prompt fails fast. The only stdin wt.py reads is the hook JSON.
- Ambiguous names are refused rather than chosen interactively: `'{want}' matches {n} worktrees:\n  {paths}`. No match: `no worktree matching '{want}'`.
- **Windows branches**
  - `run_cli`: a `.cmd` or `.bat` goes through `shell=True` with `list2cmdline` (`os.name == "nt"`).
  - `exec_shell`: pwsh, else powershell, with `-NoProfile -Command`. POSIX uses `bash -lc`.
  - Code shaped by Windows but run on every platform:
    - `remove_checkout`: the retry, and judging success by registration.
    - `release_panes`.
    - `drop_empty_dir`.
    - Explicit utf-8 decoding everywhere, because the codepage would otherwise be cp1252.
    - `symlink_to` needs developer mode on Windows; a failure there triggers rollback.
- **Concurrency**: one worker thread overlaps the naming model call with `git fetch`.

## 7. Behavior pinned by tests (`test_wt.py`)

**Pure config**
- TestConfigPrecedence:
  - `test_defaults_layer`: `[defaults]` applies.
  - `test_mode_overrides_defaults`: `[mode.x]` wins over `[defaults]`.
  - `test_provider_overrides_mode`: `[mode.x.github]` wins over `[mode.x]`.
  - `test_provider_subtables_never_leak_as_options`: no `github` key appears in the resolved conf.
  - `test_teardown_namespace_is_separate`: per-mode teardown overrides work.
  - `test_shipped_config_protects_review_trees`: the real `config.toml` gives pr/branch `copy == []`, branch `delete_branch = "never"`, hook `force = false`.
- TestDeepMerge:
  - `test_tables_merge`: nested tables merge.
  - `test_arrays_replace_not_append`: arrays replace.
- TestNamespacesAreNotOptions:
  - `test_an_unknown_sub_table_does_not_leak_into_options`: a `gitlab` sub-table is not merged in as an option.
  - `test_the_provider_sub_table_still_applies`: the matching provider sub-table still applies.
- TestShippedDefaults:
  - `test_review_modes_never_inherit_copy`: with DEFAULTS plus a repo-level `copy`, pr and branch still get `[]` while new inherits it.
- TestConfigValidation:
  - `test_a_misspelt_delete_branch_is_refused`: the message mentions "would delete the branch".
  - `test_a_misspelt_delete_branch_under_a_mode_is_refused`: the message names `[teardown.mode.pr]`.
  - `test_an_unknown_key_suggests_the_real_one`: "did you mean require_clean".
  - `test_a_quoted_number_is_refused`: "should be int".
  - `test_a_bool_is_not_an_int`: a bool is refused for `exec_timeout`.
  - `test_an_unknown_mode_is_refused`: an unknown mode is refused.
  - `test_a_bad_conventional_type_is_refused`: a bad `type_from_tracker` value is refused.
  - `test_every_problem_is_reported_at_once`: multiple problems appear in one message.
  - `test_the_shipped_config_validates`: the real `config.toml` passes.
  - `test_a_provider_sub_table_is_checked_too`: the message names `[mode.item.github]`.

**Naming and templating**
- TestSlugify:
  - `test_folds_norwegian_letters`: "Går på økt" becomes "gar-okt" (æ→ae, ø→o, å→a; "pa" is a stopword).
  - `test_drops_stopwords`: stopwords are dropped; 4 words at most.
  - `test_all_stopwords_falls_back_to_raw_words`: all stopwords falls back to the raw words.
  - `test_empty_input`: "!!!" gives "work".
- TestBranchSpec:
  - `test_accepts_full_branch_name`: a full branch name matches.
  - `test_accepts_nested`: a nested name matches.
  - `test_accepts_prefix_outside_types`: `wip/`, `infra/` match.
  - `test_rejects_prose_and_uppercase`: prose, `Feat/X`, `feat/` and `feat` are rejected.
- TestFill:
  - `test_substitutes`: placeholders are substituted.
- TestFillStrict:
  - `test_unresolved_placeholder_raises`: an unresolved placeholder dies.
  - `test_all_resolved_is_fine`: a fully resolved template is fine.
- TestLlmNameReasons:
  - `test_disabled_naming_says_so`: "not requested".
  - `test_a_missing_claude_binary_is_named`: the reason mentions "PATH".
- TestDecode:
  - `test_prefers_utf8`: utf-8 is preferred.
  - `test_falls_back_for_console_codepage`: cp1252 fallback.
  - `test_undecodable_bytes_do_not_raise`: undecodable bytes do not raise.

**Provider detection**
- TestProviderOf:
  - `test_ado_https`: org, project and repo are parsed from the https URL.
  - `test_ado_ssh`: `v3/` SSH URLs parse.
  - `test_github`: github.com is detected.
  - `test_no_remote`: no remote gives `none`.
  - `test_legacy_visualstudio_host`: the org is the host and the base is `https://myorg.visualstudio.com`.
  - `test_an_ado_host_with_an_unparseable_path_has_no_org`: gives `AdoProvider` with empty info.
  - `test_shell_metacharacters_in_the_org_are_refused`: `o;whoami` dies.
  - `test_lookalike_host_is_not_ado`: dev.azure.com inside the path does not count.
  - `test_account_url_does_not_repeat_a_legacy_org`: the account URL does not repeat a legacy org.
  - `test_ado_writes_are_skipped_when_the_origin_did_not_parse`: `AdoProvider()` writes are skipped, with no KeyError.
  - `test_unsupported_provider_refuses_lookups`: lookups die, and `link_branch` is skipped.
- TestGitHubTypeMap:
  - `test_a_configured_label_is_recognised`: a configured label is recognised.
  - `test_provider_of_carries_the_merged_map`: the merged map keeps the built-ins.

**Subprocess, HTTP, auth**
- TestExecShell:
  - `test_explicit_config_wins`: a configured shell wins.
  - `test_platform_default_is_usable`: the default shell's executable is on PATH.
- TestTokenCache:
  - `test_second_call_does_not_respawn_az`: one az spawn per process.
  - `test_a_failed_fetch_is_not_cached`: a failure is not cached.
- TestAdoRequestRetry:
  - `test_a_transient_failure_is_retried`: a URLError is retried.
  - `test_a_server_error_is_retried`: a 503 is retried.
  - `test_a_client_error_is_not_retried`: a 400 is attempted once.
  - `test_it_gives_up_and_reports_the_last_failure`: 3 attempts, then the last detail is reported.
  - `test_a_refused_redirect_names_the_host`: the redirect host appears in the detail.
- TestAlreadyLinked:
  - `test_type_key_is_recognised`: the typeKey is recognised.
  - `test_a_different_failure_is_not_a_success`: a different typeKey is not success.
  - `test_non_json_falls_back_to_the_substring`: a body that is not JSON falls back to the substring match.

**Parsing and selection**
- TestParsePorcelain:
  - `test_skips_bare_and_prunable`: bare and prunable records are skipped.
  - `test_every_record_has_head_and_branch`: every record has head and branch.
  - `test_detached_has_no_branch`: a detached record gets branch None.
  - `test_trailing_record_without_blank_line`: the last record is kept even without a trailing blank line.
- TestFindTargets:
  - `test_branch_name_is_unique`: an exact branch is unique.
  - `test_shared_leaf_is_ambiguous`: a shared leaf gives 2 matches.
  - `test_suffix_is_anchored`: "loop" and "438" do not match `fix/21438-msal-loop`.

**Teardown (these use a repo)**
- TestDirtyReasons:
  - `test_clean_tree_has_no_reasons`: a clean tree gives `[]`.
  - `test_uncommitted_changes`: detected.
  - `test_unpushed_commits`: detected.
  - `test_a_stash_elsewhere_in_the_repo_does_not_block`: a stash elsewhere does not block.
  - `test_missing_tracking_ref_is_a_reason_not_silence`: gives "could not compare".
- TestTeardownPolicy:
  - `test_refuses_without_sidecar`: refuses without a sidecar.
  - `test_sidecar_survives_a_failed_removal`: the sidecar survives a failed removal.
  - `test_a_deregistered_tree_finishes_the_teardown`: git exits non-zero but deregistered, so the sidecar is removed and the empty directory is deleted.
  - `test_refuses_dirty_tree`: a dirty tree is refused.
  - `test_force_removes_dirty_tree`: `--force` removes it.
  - `test_dry_run_changes_nothing`: dry run changes nothing.
  - `test_purges_copied_secrets`: copied secrets are purged.
  - `test_git_output_is_decoded_as_utf8_whatever_the_locale`: `blåbær` is decoded correctly under `PYTHONUTF8=0`.
  - `test_purge_ignores_paths_outside_the_worktree`: `../../../canary` survives.
- TestPurgeCopied:
  - `test_deletes_the_copied_file_and_leaves_the_rest`: only copied files go.
  - `test_deletes_a_copied_directory`: a copied directory is deleted.
  - `test_a_missing_entry_is_not_an_error`: a missing entry is not an error.
- TestDeleteBranchPolicy:
  - `test_never_keeps_the_branch`: never keeps it.
  - `test_always_drops_an_unmerged_branch`: always drops an unmerged branch.
  - `test_if_merged_keeps_an_unmerged_branch`: if_merged keeps an unmerged branch.
  - `test_if_merged_drops_a_merged_branch`: if_merged drops a merged branch.
- TestMainWorktreeProtected:
  - `test_refuses_to_remove_main`: refuses even with force.
- TestModeScopedRoot:
  - `test_sidecar_found_when_a_mode_overrides_root`: teardown finds the sidecar when `[mode.pr] root` is overridden.
- TestConfiguredRemote (remote `upstream`):
  - `test_default_branch_reads_the_configured_remote`: default_branch uses it.
  - `test_base_resolves_against_the_configured_remote`: `{remote}/{default_branch}` gives `upstream/main`.
  - `test_the_dirty_check_compares_against_the_configured_remote`: the dirty check compares against it.
- TestRemoveMerged:
  - `test_reaps_a_branch_that_has_landed`: removed. This test uses a fresh branch with no commits.
  - `test_spares_a_branch_with_unmerged_work`: spared.
  - `test_reaps_a_squash_merged_branch_whose_upstream_is_gone`: removed when the upstream is gone.
  - `test_a_branch_that_never_had_a_remote_is_not_gone`: never pushed is not "gone".
  - `test_spares_a_detached_review_tree`: a detached tree is spared.
  - `test_stale_and_merged_together_are_refused`: refused.
- TestStale:
  - `test_reaps_past_ttl`: past TTL is removed.
  - `test_spares_inside_ttl`: inside TTL is spared.
  - `test_spares_modes_without_ttl`: modes without a TTL are spared.
- TestSweepResilience:
  - `test_sweep_continues_past_a_refusal`: one refusal does not abort the sweep, and the run still exits with an error.

**Create**
- TestCreateRollback:
  - `test_a_failed_seed_leaves_no_worktree`: no tree and no branch are left.
- TestExecContract:
  - `test_defer_exec_leaves_the_steps_for_the_new_workspace`: deferred steps do not run locally.
  - `test_exec_output_never_reaches_stdout`: exec output never reaches stdout.
  - `test_a_hanging_exec_step_is_bounded`: a timeout is recorded in `exec_failed` and the tree survives.
  - `test_exec_strict_rolls_the_worktree_back`: an error, with no tree and no branch left.
- TestStalePrTree:
  - `test_refuses_when_the_pinned_base_moved`: a detached tree whose base moved is refused.
- TestPathContainment:
  - `test_refuses_to_create_outside_the_root`: a `../../../` dirname is refused.

**Commands**
- TestCd:
  - `test_resolves_a_worktree_by_leaf`: resolves by leaf.
  - `test_resolves_by_bare_name`: resolves by bare name.
  - `test_no_name_is_the_main_worktree`: no name gives the main worktree.
  - `test_an_unknown_name_is_refused`: unknown is refused.
  - `test_an_ambiguous_name_is_refused_not_guessed`: "matches 2".
- TestOpen:
  - `test_opens_the_tree_by_bare_name_and_prints_nothing`: stdout is empty, mode comes from the sidecar, and the branch keyword is passed.
  - `test_run_is_handed_to_the_workspace`: `--run` is handed to the workspace.
  - `test_an_unknown_name_is_refused`: unknown is refused.
- TestComplete:
  - `test_offers_the_leaf_and_omits_the_main_tree`: no `(main)`.
  - `test_every_name_offered_actually_resolves`: every name offered resolves with `select_one`.
  - `test_a_detached_tree_is_offered_by_its_directory`: `review/pr-7` is offered.
- TestItemPublishing:
  - `test_push_link_and_state_on_a_clean_run`: branch `fix/21438-msal-login-loop`, state set to Active.
  - `test_a_failed_push_tells_the_tracker_nothing`: no link and no state.
  - `test_a_missing_herdr_is_not_a_failure`: missing herdr is not a failure.
  - `test_a_forge_without_an_equivalent_is_not_a_failure`: a skip is not a failure.
  - `test_a_retry_reuses_the_recorded_branch`: the same path, one lookup.
- TestHooks:
  - `test_two_sessions_do_not_share_a_worktree`: two sessions get different trees.
  - `test_the_same_session_reuses_its_worktree`: the same session gets the same tree.
  - `test_prose_is_slugified_never_used_as_a_ref`: the result matches BRANCH_SPEC.
  - `test_a_full_branch_name_is_taken_verbatim`: taken verbatim.
  - `test_a_malformed_payload_creates_nothing`: nothing is created.
  - `test_a_json_array_payload_is_refused_not_crashed`: refused with a WtError, not a crash.
  - `test_remove_needs_the_exact_path_not_a_leaf`: a leaf is refused and the exact path works.
  - `test_remove_does_not_force_over_a_dirty_tree`: a dirty tree is refused.
- TestAcceptance:
  - `test_new_ls_rm_round_trip`: through `main()`, `-q new feat/acceptance-probe --no-llm` prints exactly the path `<repo>/../repo.worktrees/feat/acceptance-probe` and seeds `.env`; `ls` shows the branch and "new" and no "dirty"; `rm` removes the tree, the sidecar and the registration.

**herdr**
- TestReleasePanes:
  - `test_a_shell_pane_is_sent_back_to_the_parent_checkout`: the pane is sent `cd "<root>"`.
  - `test_a_pane_outside_the_tree_is_left_alone`: a sibling `feat/xylophone` is not touched.
  - `test_wts_own_pane_is_never_typed_into`: wt's own pane is reported, not typed into.
  - `test_an_agent_pane_is_reported_not_typed_into`: an agent pane is reported, not typed into.
- TestHerdrOpen:
  - `test_open_names_the_parents_workspace_as_the_source`: `--workspace w1`.
  - `test_a_repo_without_a_workspace_is_refused_not_guessed`: refused, and only `list` is called.
  - `test_open_does_not_steal_focus`: `--no-focus`.
  - `test_commands_are_sent_to_the_new_pane_in_order`: commands go to the new pane in order.
  - `test_a_reply_without_a_pane_is_a_failure_when_there_are_commands`: a failure when there are commands, and ok when there are none.

**Test infrastructure**
- A template repo with a bare origin is built once and copied per test.
- `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM` are set to devnull for the fixture's git calls.
- `wt.GLOBAL_CONFIG` is monkeypatched away. The fixture's `.wt.toml` is `naming.llm = false`, `copy = [".env"]`, `exec = []`.
- Monkeypatched seams: `run_cli`, `git_try`, `seed_files`, `open_workspace`, `ado_token`, `urllib.request.build_opener`, `shutil.which`, `RETRY_BASE`, `sys.stdin`. For the Rust port, these mark where the IO seams (traits) should go.

## 8. Dead code, half-finished, questionable

**Doc drift**
1. The module docstring (lines 11-20) says `wt new [<type>] <description>`, and the `config.toml` comment shows `wt new fix "msal login loop"`. The parser takes one positional, so both fail with exit 2 ("unrecognized arguments"). The type is only given through `--type`.
2. The docstring also leaves out `cd`, `open`, `complete` and `--merged`.
3. The README says the hooks are in `settings.json`; they are in `settings.local.json`.
4. The README says completion covers `rm` and `cd`; it also covers `open`.

**Config that is validated but never read**
5. `track` in OPTION_TYPES: `cmd_branch` only reads `args.track`.
6. Anything under `[mode.hook]` except teardown: hook-create resolves the `"new"` mode, so `hook_type` actually has to be set in `[defaults]` or `[mode.new]`.
7. `dirname` outside pr and branch.
8. `detach` outside branch mode (pr hard-codes it).
9. `[teardown.mode.X].fetch`: only the top level is read.
10. `teardown.force` outside `[teardown.mode.hook]`: it only matters for hook-remove.
11. `[herdr]` is not validated at all, so a typo in a label key is silently ignored.

**Duplication and small oddities**
12. `Due` and `Skipped` are identical NamedTuples.
13. `Provider.info` is unused for GitHub.
14. `NAMING_SYSTEM` asks for 2-4 words; the validator accepts 2-6.
15. The ADO PR error message says "merge commit", but the code reads `lastMergeSourceCommit`, which is the source branch head.
16. `--type` and `--slug` are unvalidated: an arbitrary type or a slug with spaces or slashes goes straight into the branch name.
17. `fill` dies on any literal `{` in an exec command, for example a PowerShell `{ }` script block, or a `{}` in a shell command.

**Inconsistent outcomes**
18. A failed herdr open changes the exit code only for `item`. `new`, `pr` and `branch` just log it, and `wt open` exits 0 even when `ok` is false.
19. On `item`, once any post-create step fails, the path is not printed, so the shell wrapper does not cd even though the tree exists.

**Unguarded crashes (tracebacks instead of `wt:` errors)**
20. A sidecar that is valid JSON but not an object passes `iter_meta` and then fails on `.get`.
21. A malformed `created` makes `fromisoformat` raise in both `ls` and `--stale`.
22. `seed_files` failures are re-raised as a raw `OSError`.
23. Exceptions from `llm_name` other than a timeout (for example `OSError`) propagate through the Future.

**Hazards**
24. **`wt rm --merged` reaps fresh branches**: `git branch --merged` includes any branch with no new commits, so a brand-new, unused worktree gets reaped and `if_merged` deletes its branch. `require_clean` only protects uncommitted changes. The test `test_reaps_a_branch_that_has_landed` depends on exactly this.
25. **ADO projects with spaces**: the org/project/repo check `[A-Za-z0-9._ -]+` rejects `%20`. An ADO project with a space in its name is URL-encoded in the remote, so `provider_of` dies on every new, item, pr, branch and hook-create in that repo.
26. **Unhandled ADO URL shapes**: legacy `visualstudio.com/DefaultCollection/...` and `vs-ssh.visualstudio.com` remotes silently become `AdoProvider({})`, so lookups run without `--org` and writes are skipped.
27. **GitHub Enterprise** hosts fall to provider `none`.
28. **Non-deterministic GitHub type**: the type comes from iterating a **set** of labels, so an issue carrying two mapped labels (for example "bug" and "feature") can get a different type from run to run, because string hashing is randomized per process.
29. **Fork PRs**: `worktree add --detach <sha>` assumes the fetch brought the sha in. A GitHub PR from a fork is not fetched (there is no `refs/pull/*` fetch), so the add fails.

**Logic issues**
30. `default_branch` takes `rsplit("/", 1)[-1]`, so a default branch with a slash in it (for example `release/main`) is truncated.
31. `cmd_branch` with `detach = false` in config and no `--track`:
    - It builds `worktree add -b <name> <path> <name>`, whose base is the not-yet-existing local name, so it fails unless that ref resolves.
    - `dirname = name` is also unfilled in that case.
32. `cmd_branch` detached: `reuse_existing` compares `base` strings (`origin/x` against `origin/x`), so a remote branch that has moved on is never detected. The stale-pin guard only really works for pr, whose base is a sha.
33. `branch_for_item` trusts any sidecar, including one left behind after a manual `git worktree remove`.
34. PR sidecars also record `id` (the PR number), which shares a key space with work item ids. It is harmless today only because PR sidecars have branch null.
35. `seed_files` does not check that `copy` and `symlink` rel paths stay inside the tree (purge does), so `copy = ["../x"]` writes outside the worktree.
36. Symlinked entries are not recorded in `copied`.
37. `leaf_of` resolves with provider `"none"`, so a `[mode.X.ado] root` override is not honoured by `ls` and `complete`, which then fall back to the bare name.
38. `hook-remove` matches `str(resolved path) == str(payload path)` exactly, so any separator or case difference coming from Claude makes it fail with "no worktree at".
39. `cmd_rm` fetches (when `teardown.fetch` is on) before checking `--stale` together with `--merged`.
40. `plan_removal` dies on a missing sidecar before the dry-run report. The comment "so --dry-run can preview any tree" does not hold without `--force`.
41. `ls` treats a failed `git status` as clean (fails open), which contradicts the fail-closed rule in `dirty_reasons`.
42. `release_panes` types `cd "{root}"`. A `$` or `"` in the root path would be interpreted by the pane's shell.
43. `write_meta` is called twice when exec steps fail. With `exec_strict`, the sidecar is written and then deleted by the rollback.
44. The ADO retry leans on the server rejecting duplicate relations; the code says so in a comment.

## Files
- `~/.config/wt/wt.py`
- `~/.config/wt/test_wt.py`
- `~/.config/wt/config.toml`
- `~/.config/wt/shell/wt.ps1`
- `~/.config/wt/README.md`
- `~/.config/wt/.githooks/pre-commit`
- `~/.local/bin/wt.cmd`
- `~/.local/bin/wt`
- `~/.claude/settings.local.json` (the WorktreeCreate/WorktreeRemove hooks)