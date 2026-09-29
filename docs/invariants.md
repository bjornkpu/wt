# Invariants

Rules that must always hold. Each has an id, one sentence, and the test that pins it. Never
break one. An invariant without a test is a bug: write the test or delete the invariant.
Ids are never reused.

| Id | Invariant | Pinned by |
| --- | --- | --- |
| INV-1 | stdout carries only command output: a create verb prints its path and nothing else, never logs or exec step output. | `tests/new.rs::test_new_prints_the_exact_path_seeds_and_ls_shows_it`, `tests/new.rs::test_exec_output_never_reaches_stdout` |
| INV-2 | A create that fails part-way leaves no worktree behind. | `tests/new.rs::test_a_failed_seed_leaves_no_worktree`, `tests/new.rs::test_exec_strict_rolls_the_worktree_back` |
| INV-3 | wt never creates a worktree outside its configured root. | `tests/new.rs::test_refuses_to_create_outside_the_root` |
| INV-4 | wt never creates a worktree over a directory that is already there, and leaves that directory untouched. | `tests/new.rs::a_stray_non_empty_directory_is_refused_untouched`, `tests/new.rs::a_stray_empty_directory_is_refused_untouched` |
| INV-5 | Prose given to the `WorktreeCreate` hook becomes a slugified branch name, never a raw ref. | `tests/hooks.rs::test_prose_is_slugified_never_used_as_a_ref` |
| INV-6 | A name that matches more than one worktree is refused, never guessed. | `tests/cd_complete.rs::test_an_ambiguous_name_is_refused_not_guessed` |
| INV-7 | Without `--force`, `rm` refuses a tree with uncommitted changes or unpushed commits. | `tests/rm.rs::test_refuses_dirty_tree`, `tests/rm.rs::test_unpushed_commits` |
| INV-8 | `rm` never removes the main worktree, not even with `--force`. | `tests/rm.rs::test_refuses_to_remove_main` |
| INV-9 | The `WorktreeRemove` hook removes only the tree at the exact path it is given, and never over uncommitted work. | `tests/hooks.rs::test_remove_needs_the_exact_path_not_a_leaf`, `tests/hooks.rs::test_remove_does_not_force_over_a_dirty_tree` |
| INV-10 | Without `--force`, a branch with unpushed commits is never deleted, even under `delete_branch = "always"`. | `tests/prunable.rs::delete_branch_always_refuses_a_hand_deleted_tree_with_unpushed_commits` |
| INV-11 | A `delete_branch` value wt does not recognise is a config error, never read as a delete. | `src/domain/config.rs::tests::test_a_misspelt_delete_branch_is_refused` |
| INV-12 | `rm --dry-run` changes nothing. | `tests/rm.rs::test_clean_tree_dry_run_changes_nothing` |
| INV-13 | `item` links the branch and sets the tracker state only after the push succeeds. | `tests/item.rs::test_a_failed_push_tells_the_tracker_nothing` |
| INV-14 | When `WT_HOME` is set, config and logs live under it and nowhere else. | `src/domain/paths.rs::tests::wt_home_overrides_both` |
