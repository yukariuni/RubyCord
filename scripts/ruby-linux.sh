#!/bin/sh
# Installs Ruby for Desktop Phoenix on Linux: a launcher that preloads the
# loader into Discord, and a desktop entry for it. Discord's own files stay
# untouched, so its updates never undo Ruby.
#
# CI publishes this script with libphoenix.so appended after the __PAYLOAD__ line, as the one
# file people download (ruby_discord_client-linux):
#   chmod +x ruby_discord_client-linux && ./ruby_discord_client-linux
#   ./ruby_discord_client-linux /path/to/Discord.AppImage
#   ./ruby_discord_client-linux --no-runtime
#   ./ruby_discord_client-linux --uninstall
# From a checkout, RUBY_LOADER=/path/to/libphoenix.so sh scripts/ruby-linux.sh does the same.
set -eu

data=${XDG_DATA_HOME:-$HOME/.local/share}
home=$data/ruby
entry=$data/applications/ruby.desktop
gate_entry=$data/applications/ruby-gate-only.desktop

if [ "${1:-}" = --uninstall ]; then
   rm -rf "$home" "$entry" "$gate_entry"
   echo "Ruby is uninstalled. Discord itself was not changed."
   exit 0
fi

no_runtime=
while [ "${1:-}" = --no-runtime ] || [ "${1:-}" = --gate-only ]; do
   no_runtime=1
   shift
done

# LD_PRELOAD splits on spaces and colons, and the desktop entry's Exec= would
# need escaping for the rest.
case $home in
   *[[:space:]:\"\$\`\\\']*)
      echo "Ruby can't be installed under $home: set XDG_DATA_HOME to a path without spaces, colons or quotes." >&2
      exit 1
      ;;
esac

script=$0
discord=${1:-}
if [ -n "$discord" ] && [ ! -f "$discord" ]; then
   echo "No file at $discord. Pass the path of Discord Phoenix's AppImage:" >&2
   echo "  $script /path/to/discord-desktop.AppImage" >&2
   exit 1
fi
if [ -z "$discord" ]; then
   discord=$(command -v discord-phoenix 2>/dev/null || true)
fi
if [ -z "$discord" ]; then
   # The newest AppImage wins when an older download is still around.
   discord=$(ls -t "$PWD"/*[Dd]iscord*.AppImage "$HOME"/Applications/*[Dd]iscord*.AppImage \
      "$HOME"/Downloads/*[Dd]iscord*.AppImage "$HOME"/Desktop/*[Dd]iscord*.AppImage \
      2>/dev/null | head -n 1 || true)
fi
if [ -z "$discord" ]; then
   cat >&2 <<EOF
Couldn't find Discord Phoenix. Looked for discord-phoenix on PATH and for a
Discord AppImage in $PWD, ~/Applications, ~/Downloads and ~/Desktop.

Download Discord Phoenix's AppImage, then run this again with its path:
  $script /path/to/discord-desktop.AppImage
EOF
   exit 1
fi
discord=$(realpath "$discord")
[ -x "$discord" ] || chmod +x "$discord"
echo "Using Discord Phoenix at $discord"

# Single-quotes a value for the generated launcher.
q() { printf "'%s'" "$(printf %s "$1" | sed "s/'/'\\\\''/g")"; }

mkdir -p "$home" "$data/applications"
# The launcher preloads the loader installed beside it.
cat >"$home/ruby" <<EOF
#!/bin/sh
home=$(q "$home")
while [ "\${1:-}" = --no-runtime ] || [ "\${1:-}" = --gate-only ]; do
   export RUBY_GATE_ONLY=1
   shift
done
# A second Discord hands off to the running one and exits, so one running
# without Ruby is restarted with it.
discord=$(q "$discord")
pids= ruby=
for p in /proc/[0-9]*; do
   if [ "\$(readlink "\$p/exe" 2>/dev/null)" = "\$discord" ] ||
      tr '\\0' '\\n' <"\$p/environ" 2>/dev/null | grep -qxF "APPIMAGE=\$discord"; then
      pids="\$pids \${p#/proc/}"
      grep -q libphoenix "\$p/maps" 2>/dev/null && ruby=1
   fi
done
if [ -n "\$pids" ] && [ -z "\$ruby" ]; then
   kill \$pids 2>/dev/null
   for _ in 1 2 3 4 5 6 7 8 9 10; do
      alive=
      for pid in \$pids; do kill -0 "\$pid" 2>/dev/null && alive=1; done
      [ -z "\$alive" ] && break
      sleep 0.5
   done
fi
[ -f "\$home/libphoenix.so" ] &&
   export LD_PRELOAD="\$home/libphoenix.so\${LD_PRELOAD:+:\$LD_PRELOAD}"
exec "\$discord" "\$@"
EOF
chmod +x "$home/ruby"

exec_args=
[ -n "$no_runtime" ] && exec_args=" --no-runtime"
cat >"$entry" <<EOF
[Desktop Entry]
Type=Application
Name=Ruby
Comment=Discord with Ruby
Exec="$home/ruby"$exec_args %U
Icon=discord
Terminal=false
Categories=Network;InstantMessaging;
MimeType=x-scheme-handler/discord;
EOF
# For when the plugin runtime crashes on a Discord build it doesn't know yet.
cat >"$gate_entry" <<EOF
[Desktop Entry]
Type=Application
Name=Ruby (access gate only)
Comment=Discord past the access gate, without plugins
Exec="$home/ruby" --no-runtime %U
Icon=discord
Terminal=false
Categories=Network;InstantMessaging;
EOF
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true

# The loader: appended to this file by CI, or named by RUBY_LOADER.
rm -f "$home/libphoenix.so"
if [ -n "${RUBY_LOADER:-}" ]; then
   cp "$RUBY_LOADER" "$home/libphoenix.so"
else
   start=$(grep -a -n -m1 '^__PAYLOAD__$' "$0" | cut -d: -f1 || true)
   if [ -z "$start" ]; then
      echo "This copy has no loader attached. Download ruby_discord_client-linux from the releases, or set RUBY_LOADER." >&2
      exit 1
   fi
   tail -n +"$((start + 1))" "$0" >"$home/libphoenix.so"
fi
chmod +x "$home/libphoenix.so"
echo "Ruby is installed. Open \"Ruby\" from your app menu to start Discord with it."
echo "It runs Discord from $discord: if you move or replace that file, run this again."
exit 0
