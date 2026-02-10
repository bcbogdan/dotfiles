export PATH="$HOME/.local/bin:$PATH"
export ZSH="$HOME/.oh-my-zsh"
ZSH_THEME="robbyrussell"

plugins=(git)
[[ -d "$ZSH/custom/plugins/zsh-autosuggestions" ]] && plugins+=(zsh-autosuggestions)
[[ -f "$ZSH/oh-my-zsh.sh" ]] && source "$ZSH/oh-my-zsh.sh"

command -v fzf >/dev/null && source <(fzf --zsh)

gitfc() {
  git checkout development && git pull origin development --ff-only && git checkout -b "$1"
}

gitclean() {
  git branch --merged | egrep -v "(^\*|master|main|dev)" | xargs git branch -d
}

gitupdate() {
  CURRENT_BRANCH=$(git branch --show-current)
  git checkout development
  git pull origin development
  git checkout "$CURRENT_BRANCH"
}

export PYENV_ROOT="$HOME/.pyenv"
if command -v pyenv >/dev/null; then
  eval "$(pyenv init -)"
fi

[ -s "$HOME/.bun/_bun" ] && source "$HOME/.bun/_bun"

export BUN_INSTALL="$HOME/.bun"
export PATH="$BUN_INSTALL/bin:$PATH"
[[ -d /opt/homebrew/opt/mysql-client/bin ]] && export PATH="/opt/homebrew/opt/mysql-client/bin:$PATH"

command -v mise >/dev/null && eval "$(mise activate zsh)"
command -v go >/dev/null && export PATH="$(go env GOPATH)/bin:$PATH"

[[ -f "$HOME/.atuin/bin/env" ]] && source "$HOME/.atuin/bin/env"
command -v atuin >/dev/null && eval "$(atuin init zsh)"

[[ -f "$HOME/.local/bin/env" ]] && source "$HOME/.local/bin/env"
command -v zoxide >/dev/null && eval "$(zoxide init zsh)"

if [[ $(uname -s) == Linux ]]; then
  export DOCKER_HOST="unix://${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/docker.sock"
fi

export PATH="$HOME/.tfenv/bin:$PATH"
export PATH="$HOME/.npm-global/bin:$PATH"

[ -f "$HOME/.zshrc.local" ] && source "$HOME/.zshrc.local"
