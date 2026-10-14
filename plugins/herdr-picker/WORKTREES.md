# Worktrees

On the Zoxide tab, select a directory and press **Shift+Enter**. The picker discovers
the Git repository (including nested directories and linked worktrees), asks for a
new branch name, a local or cached remote base branch, and the full destination
path. No fetch is performed. Review the resolved commit and path, then press Enter
on **Create & open**.

The selected folder's **machine badge determines the execution host**. Ctrl+M or
`machine:goblin` filters folders first; it never redirects an existing row. The
worktree form shows the host in its title. Git discovery, branch validation,
destination expansion (`~` uses the destination host's HOME), filesystem completion
and creation run on that host through the versioned helper. See
[MULTI_MACHINE.md](MULTI_MACHINE.md) for setup and protocol details.

**Create Worktree** is a centered, content-sized inner form (up to 80 columns,
normally 18–24 rows). Name, Base, and Path remain visible; the active field has a
highlighted border and cursor. Suggestions occupy at most five rows. The outer
Herdr popup is not resized or retitled: Herdr 0.8.2 exposes neither operation for
its singleton popup.

The suggested path is `<repository-parent>/<repository-name>-<branch-name>`, with
slashes replaced by hyphens only in the directory suggestion. Relative paths use
the discovered source repository's parent (the selected worktree for linked
repositories); `~` and `~/…` expand to HOME. Existing destinations
are rejected. A detached HEAD has no default base: select one with arrow keys.

Ctrl+W deletes the whitespace-delimited word before the cursor, including trailing
whitespace, in all three inputs.

Path completion uses cached Zoxide entries for bare search text and filesystem
directories for paths (`./`, `/`, `~/`, or text containing `/`). Up/Down highlights
a suggestion; Enter accepts it before advancing. Tab accepts the highlighted or
first suggestion. Home/End, Left/Right, Backspace/Delete, and Ctrl+U edit fields.
Tab advances Name and Base after asynchronous validation; Shift+Tab goes to the
previous field. On Path, Tab remains completion and Enter advances to review.
Esc goes back or cancels without changing the picker's query, tab, or sort.

Creation runs off the UI thread with a 120-second Git timeout. While creating or
opening, repeated keys cannot start another operation. Failed/uncertain creation
is reconciled against the registered worktree, branch, commit and clean checkout.
No automatic deletion or rollback occurs. **Retry opening** only retries the
workspace operation, refreshing workspace identity first; it never recreates Git
state. Successful opening records directory/workspace history and best-effort
`zoxide add`.

For remote worktrees on official Herdr, successful opening returns to the picker
with a message to select that machine in Herdr's sidebar. With the optional fork's
injected client token, opening navigates the viewing client and closes the picker
after acknowledgement. Failed navigation retains the created workspace ID for
manual retry; a timeout has unknown outcome and does not cancel navigation. See
`MULTI_MACHINE.md` for the fork requirements and CLI contract.
A failed open remains **Retry opening**, distinct from uncertain Git creation;
retry refreshes workspaces only on the original host and never recreates the
worktree. SSH creation timeouts are treated as uncertain and require rechecking.

## Keyboard compatibility

The picker queries Crossterm keyboard-enhancement support and, when advertised,
pushes escape-code disambiguation flags, popping them on exit. Shift+Enter still
depends on the terminal and Herdr forwarding a distinct key event. This has **not
been verified in a live Herdr pane**; advertising protocol support alone does not
prove end-to-end forwarding. Legacy terminals that send ordinary Enter for both
keys cannot distinguish this shortcut.

Completion has no debounce. Zoxide searches and edits within a cached directory
filter immediately in memory. Following OpenCode's directory-search approach,
listings are keyed by parent directory, retaining up to 4096 entries before the
40-result limit. Exact and prefix basename matches rank before fuzzy matches,
with deterministic ties. The per-flow cache holds 16 directories; failed scans
expire after one second. Directory entry types avoid per-entry metadata calls
except for symlinks. One shared gate bounds scans even across cancellation;
in-flight results populate the cache but are always filtered against the current
input, so stale suggestions cannot replace current results. Worker polling uses
16ms, and background live previews pause during the form. Filesystem calls run
off-thread but cannot be forcibly interrupted on a stalled network mount.
The existing command runner returns text with lossy UTF-8 conversion, so paths
containing non-UTF-8 bytes are not fully supported by discovery or editing.

Remote directory listings are cached per form/host using opaque input-directory
keys; `~/…` is expanded only by the remote helper. Filtering cached listings is
immediate and late replies filter the latest input. Uncached scans are bounded
SSH jobs behind a per-host completion gate. Bare Zoxide completion runs against
that host's cached Zoxide paths. No remote completion reads the coordinator's
filesystem.
