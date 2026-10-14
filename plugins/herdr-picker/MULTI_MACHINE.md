# Multiple machines

The picker shows one machine at a time, starting with its executing host.
Run it on the machine holding your client catalog (typically the Mac). Plugins
execute on the **selected server**: opening the plugin while attached to another
server uses that server's catalog. A host without saved profiles works locally.

## Picking a host

- The active machine name and shortcut appear above the tabs.
- **Ctrl+M** cycles the executing host → enabled saved profiles → executing host.
  There is no combined view. The selected machine applies across all three tabs;
  search text is preserved when switching.
- `machine:goblin project` can further narrow the current machine's results;
  it does not switch machines. Matching is case-insensitive against label or ID.
- **Enter** focuses/opens on the selected row's host. **Shift+Enter** on a Zoxide
  directory creates a worktree on that same host. Filtering never changes routing.

Ctrl+M, like Shift+Enter, needs distinct key events from the terminal and Herdr.
Legacy terminals may encode Ctrl+M as Enter; **Ctrl+G** is an equivalent shortcut.
Other machines refresh and cache in the background so switching stays quick.

With the optional fork's `client_navigation_v1` capability, remote success switches
the viewing client and closes the popup after activation, focus and display sync
are acknowledged. Without its injected `HERDR_CLIENT_TOKEN`, official Herdr keeps
the legacy behavior: focus/open the destination server, keep the popup open, and
explicitly ask you to select that machine in Herdr's sidebar. `--machine … workspace
focus` alone does not change the client's selected machine. Remote failures keep
the popup open without recording successful access. Local actions retain their
existing close-and-focus behavior, including when executing on a remote server.

## Optional fork navigation

The originating CLI (`HERDR_BIN_PATH`), running origin server and viewing client
must support the fork; the destination server may run official Herdr 0.9.3. The
fork injects `HERDR_CLIENT_TOKEN` only for interactive launches. A nonempty token
selects navigation support; the picker does not probe or fall back after navigation
errors. Both that token and `HERDR_SOCKET_PATH` are inherited by the origin CLI:

```text
$HERDR_BIN_PATH client navigate PROFILE_ID --workspace WORKSPACE_ID
$HERDR_BIN_PATH client navigate PROFILE_ID --pane PANE_ID
```

`PROFILE_ID` is the exact saved profile ID, not a label or SSH target. These calls
never use `--machine` or a destination session. Local rows still focus the executing
server directly; they are not mapped to the viewing client's `local` profile.

Remote directories first reuse a matching workspace or create one without
`--focus`, then navigate to its ID. If navigation fails, that ID is retained across
async client clones for a manual retry during this picker run, avoiding duplicate
creation. Worktrees use the same confirmation path. Navigation allows 40 seconds
for the API's up-to-30-second acknowledgement. A timeout is an **unknown outcome**,
not cancellation: check Herdr before manually retrying. There is no automatic
navigation retry or silent remote-focus fallback.

## Setup and local labels

Install the matching picker binary/plugin on each host. Save and enable profiles
using Herdr's supported `machine` commands. The picker reads `machine list --json`;
it does not create profiles or edit Herdr's configuration.

Remote badges use profile labels exactly, e.g. `goblin`. The executing host label
uses, in order:

1. Nonempty `HERDR_PICKER_MACHINE_LABEL`.
2. The trimmed contents of `$HERDR_PLUGIN_CONFIG_DIR/local-machine-label`.
3. The OS hostname, with a trailing `.local` removed.

`local-machine-label` is a plain UTF-8 text file, not JSON. For example, put
`goblin` followed by a newline in the remote plugin's config directory so a picker
launched directly there also displays `goblin`, even if its hostname differs.
Label overrides affect display/filtering; identities use the profile ID/session.

## Execution and failures

Local API calls continue to use injected `HERDR_BIN_PATH`. Machine catalog and
forwarding use installed Herdr, independently of that server-provided path.
Protocol mismatches and catalog failures are visible, never interpreted as an
empty successful list. Installed executables are found in `~/.local/bin`, mise
shims, Cargo's bin directory, conventional Homebrew/system locations, then PATH.
Zoxide lookup uses these locations for both direct popups and SSH helpers, so a
server started with a minimal SSH PATH does not need interactive-shell setup.

Each enabled host has an independent refresh worker, with one running and at most
one queued request. Local refresh and preview have separate workers; remote previews
have independent per-host latest-request workers. Slow/offline
hosts cannot hold up local refresh. Failed host sources are removed from the
actionable list; disabled/removed profiles lose their rows. Host errors remain
visible below the panels; filtering by machine isolates its status. Catalog
changes and worker epochs reject results from obsolete hosts/jobs. Before routed
commands, the profile ID, SSH target, session and enabled state are revalidated.

Pane snapshots, workspace creation and legacy focus use `herdr --machine <opaque-ID>`;
fork workspace/agent navigation instead uses the originating CLI as described above.
No remote failure retries locally. Remote previews are snapshots, while local
panes retain live observation. Directory matching, row IDs, previews and history
are machine-qualified, so identical paths and pane/workspace IDs do not collide.

## Faster repeat opens and refreshes

After the current machine catalog arrives, saved remote listings appear immediately
with **cached · refreshing (actions unavailable)** status. Search and selection work
while each host refreshes independently. Enter, Shift+Enter and previews wait for
live results; no cached workspace/pane ID is sent to a host. Matching selected row
keys survive the update. Failed sources replace cached rows with the normal error.

Successful sources are cached for at most 24 hours, bound to the exact enabled
profile (ID, label, SSH target and session). Removed/disabled/changed profiles never
restore old rows. Only listing data and profile metadata are saved, not credentials,
pane contents, helper executable paths or error output. Malformed/partial/unknown
cache files are ignored. Atomic private (0600) files are bounded to 4 MiB, 64 hosts
and 1,000 total rows per host; oversized host listings are not cached.

The file is `$HERDR_PLUGIN_STATE_DIR/remote-listings-v1.json`. Without plugin state,
it uses `$XDG_CACHE_HOME/herdr-picker`, or `~/Library/Caches/herdr-picker` on macOS
and `~/.cache/herdr-picker` elsewhere. Cache write failures do not block listings.

Each host worker reuses its discovered helper across the three-second refreshes.
A failed load invalidates discovery; the next refresh discovers/handshakes again.
Profile changes replace the worker. There is no prefetch daemon.

## Host-local helper

The coordinator discovers the remote `local.unified-picker` root and picker
executable via `herdr --machine <ID> plugin list --json`. It calls that executable
over SSH with `--helper-v1 <base64-JSON>`, verifies a versioned handshake, and uses
typed JSON requests/responses. Executable and payload are separately shell-quoted;
paths and Git data are never interpolated into commands. SSH uses BatchMode,
ConnectTimeout, connection attempts and keepalive limits, with bounded subprocess
timeouts. No interactive/login shell is needed.

Protocol 1 requests contain `version`, `session`, and `operation`; responses contain
`version` and a serialized `Result` (`Ok`/`Err`). Operations cover handshake, host
data load, directory summary, Zoxide add, and typed worktree operations. The helper
passes the profile's session to Herdr and augments its own PATH with host-local
mise shims and `~/.local/bin` before running tools.

Zoxide, filesystem reads, `~` expansion, symlink resolution, Git discovery,
validation and creation all run on the destination. The coordinator treats remote
paths as opaque strings; its directory caches contain remote listings scoped to
the current host/form. A helper/protocol mismatch requires installing the same
picker release on that host. The picker reports the problem rather than falling
back to local filesystem or API operations.

## History and rollback

History now writes `picker-history-v2.json` in `HERDR_PLUGIN_STATE_DIR`. On first
load it imports `picker-history.json` without modifying that legacy file. Local
keys retain their old meaning; remote keys add stable profile-ID/session namespaces.
An older picker can still read its original v1 file without quarantining new data.
History recorded after migration is not copied back to v1 during rollback.

## Verification

`cargo fmt --check`, `cargo check`, `cargo test`,
`cargo clippy --all-targets -- -D warnings`, and `cargo build --release --locked`.
Tests cover cross-host collisions, filtering/cycling, routing without fallback,
SSH quoting/session payloads, protocol mismatch, independent refresh/failures,
stale worker epochs, remote path/completion behavior and v1 history import.

After deploying matching binaries, verify local and remote rows, Ctrl+M/query
intersection, remote snapshots, offline errors, Enter and Shift+Enter in a
disposable repository. On official Herdr, confirm success requires manual sidebar
selection. With the fork on the origin server/CLI and viewing client, confirm Enter
and worktree opening switch the viewing client and close the picker only after
acknowledgement. Verify an unavailable destination retains the error UI and that
manual retry after a failed directory switch reuses the created workspace.
