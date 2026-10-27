# Isolated Herdr home for the release clips. Source it with REPO set to the repo root.
# The home is a fixed path directly under /tmp: Herdr's socket path must fit the 104-byte Unix socket limit, and a
# stable path keeps the path-derived workspace colors the same on every recording.

HERDR_BIN_DIR=$(dirname "$(mise which herdr)")
ZOXIDE_BIN_DIR=$(dirname "$(mise which zoxide)")
export PATH="$HERDR_BIN_DIR:$ZOXIDE_BIN_DIR:$PATH"
export MISE_DATA_DIR="$HOME/.local/share/mise"

export HOME=/tmp/helm-demo
if [ -S "$HOME/.config/herdr/herdr.sock" ]; then
  HERDR_SOCKET_PATH="$HOME/.config/herdr/herdr.sock" herdr server stop >/dev/null 2>&1
fi
rm -rf "$HOME"
mkdir -p "$HOME"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
export _ZO_DATA_DIR="$XDG_DATA_HOME/zoxide"
unset HERDR_ENV HERDR_SESSION HERDR_SOCKET_PATH

mkdir -p "$XDG_CONFIG_HOME/herdr/plugins/config/helm-herdr" "$_ZO_DATA_DIR"
cp "$REPO/docs/assets/tapes/herdr.toml" "$XDG_CONFIG_HOME/herdr/config.toml"
if [ "$CLIP_SCHEME" = light ]; then
  printf '\n[theme]\nname = "catppuccin-latte"\n' >> "$XDG_CONFIG_HOME/herdr/config.toml"
fi
cp "$REPO/docs/assets/tapes/helm.toml" "$XDG_CONFIG_HOME/herdr/plugins/config/helm-herdr/config.toml"

demo_commit() {
  GIT_AUTHOR_DATE="$2" GIT_COMMITTER_DATE="$2" git -C "$HOME/code/$1" \
    -c user.name=demo -c user.email=demo@example.com commit -q --allow-empty -m "$3"
}

for project in api web docs infra; do
  mkdir -p "$HOME/code/$project"
  git -C "$HOME/code/$project" init -q -b main
done
now=$(date +%s)
demo_commit api "@$((now - 300))" "feat: add the orders endpoint"
demo_commit web "@$((now - 7200))" "fix: keep the cart across reloads"
demo_commit docs "@$((now - 259200))" "docs: describe the deploy"
printf "PROMPT='%%F{8}%%1~%%f ❯ '\n" > "$HOME/.zshrc"
source "$HOME/.zshrc"

for dir in "$HOME/code/api" "$HOME/code/web" "$HOME/notes" "$HOME/dotfiles"; do
  mkdir -p "$dir"
  zoxide add "$dir"
done

cd "$HOME/code/api"
