herdr() {
  local argument previous remote_target has_remote_keybindings
  for argument in "$@"; do
    if [[ "$previous" == --remote ]]; then
      remote_target=$argument
    fi
    if [[ "$argument" == --remote-keybindings ]]; then
      has_remote_keybindings=true
    fi
    previous=$argument
  done

  if [[ "$remote_target" == goblin && "$has_remote_keybindings" != true ]]; then
    command herdr "$@" --remote-keybindings server
  else
    command herdr "$@"
  fi
}
