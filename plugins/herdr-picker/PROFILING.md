# Profiling the picker

## Preview modes

Live preview is enabled by default. Disable it at startup to use the full popup
for the picker list and avoid all observer and snapshot subprocesses:

```bash
./target/release/herdr-picker --open workspaces --preview off
./target/release/herdr-picker --open agents --preview off
```

The plugin also exposes the `open-workspaces-no-preview` and
`open-agents-no-preview` actions for permanent key bindings. Set
`HERDR_PICKER_PREVIEW=off` as an alternative to the command-line option.

Profiling is disabled by default. Set `HERDR_PICKER_PROFILE` to `1` to write a
JSONL trace under `HERDR_PLUGIN_STATE_DIR`, falling back to the system temporary
directory when that variable is absent, or set it to an explicit output path.
The `--open` launcher forwards this variable to the picker pane.

## Goblin timing trace

Build and open a profiled picker from a Herdr shell on Goblin:

```bash
cd ~/src/dotfiles/plugins/herdr-picker
cargo build --release --locked
trace="/tmp/herdr-picker-profile-$(date +%s).jsonl"
HERDR_PICKER_PROFILE="$trace" \
  ./target/release/herdr-picker --open workspaces
```

Exercise one scenario at a time, then close the picker normally so its buffered
events are flushed. Forced termination can leave an incomplete trace. Use a
different output path for each run because an existing file is truncated.
Useful scenarios are idle for ten seconds, moving one row at a time, holding an
arrow key, switching tabs, and waiting for the first preview.

Summarize recorded spans by maximum and average duration:

```bash
jq -s '
  [.[] | select(.duration_us != null)]
  | group_by(.event)
  | map({
      event: .[0].event,
      count: length,
      max_us: (map(.duration_us) | max),
      avg_us: ((map(.duration_us) | add) / length)
    })
  | sort_by(.max_us)
  | reverse
' "$trace"
```

Inspect interaction and preview latency directly:

```bash
jq 'select(.event == "input.to_draw" or .event == "preview.first_frame" or .event == "preview.frame_delivery")' \
  "$trace"
```

The most relevant events are `input.to_draw`, `preview.observer_stop`,
`preview.observer_spawn`, `preview.first_frame`, `preview.frame_delivery`, `preview.queue_wait`,
`preview.fallback`, `command.run`, `model.filtered`, `path.canonicalize`,
`preview.frame_vt100`, `preview.frame_serialize`, `ui.parse_ansi`, `ui.draw`,
and the `history.*` events. `profile.dropped` means the bounded writer queue
filled. Reduce event rate or write to faster local storage before repeating the
capture. Traces contain paths, command arguments, and pane/item identifiers;
review them before sharing.

## System profiling

Goblin does not currently permit unprivileged `perf` sampling. Run the picker
directly as a child of `strace` instead; attaching later is blocked by
`ptrace_scope=1`:

```bash
strace -f -tt -T -o /tmp/herdr-picker.strace \
  ~/src/dotfiles/plugins/herdr-picker/target/release/herdr-picker
```

This runs in the current pane rather than a plugin popup and may not have
plugin-only environment variables such as `HERDR_PLUGIN_STATE_DIR`; use it for
syscall diagnosis rather than direct latency comparisons. In another SSH
session, list picker processes, select the interactive picker PID, then observe
CPU, task switching, and I/O:

```bash
pgrep -a -x herdr-picker
pidstat -urdw -p "$picker_pid" 1
```

Child `herdr` processes are reported separately from the selected picker PID.

For symbols in a future sampling profile, keep the release optimizations but
build without stripping:

```bash
CARGO_PROFILE_RELEASE_DEBUG=1 CARGO_PROFILE_RELEASE_STRIP=false \
  cargo build --release --locked
```
