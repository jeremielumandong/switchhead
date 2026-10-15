switchhead gives read-only access to one git repository, the one the user opened this
assistant from.

- `status` shows the branch, its upstream, staged, unstaged and untracked files, and any merge
  or rebase in progress.
- `log` lists commits (newest first) for a revision, `all` branches, or one path.
- `show` gives one commit's message, stats and patch.
- `diff` compares the work tree with the index (`unstaged`), the index with HEAD (`staged`),
  or two revisions (`range`), optionally for one path.
- `branches` lists local and remote branches with upstreams and ahead/behind counts.
- `blame` shows who last changed each line of a file (optionally a line range).
- `file_at` gives a file's content at a revision.
- `conflicts` lists conflicted files, or for one file its work-tree content with conflict
  markers and the base, ours and theirs versions.

Nothing here changes the repository. Long outputs are cut and say so; narrow them with
`path`, `max` or line ranges.
