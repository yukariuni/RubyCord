#!/bin/sh
# Installs RubyCord for Desktop Phoenix on Linux: a launcher that preloads the
# loader into Discord, and a desktop entry for it. Discord's own files stay
# untouched, so its updates never undo RubyCord.
#
# CI publishes this script with libphoenix.so appended after the __PAYLOAD__ line, as the one
# file people download (rubycord-installer-linux):
#   chmod +x rubycord-installer-linux && ./rubycord-installer-linux
#   ./rubycord-installer-linux /path/to/Discord.AppImage
#   ./rubycord-installer-linux --no-runtime
#   ./rubycord-installer-linux --uninstall
# From a checkout, RUBYCORD_LOADER=/path/to/libphoenix.so sh scripts/rubycord-linux.sh does the same.
set -eu

data=${XDG_DATA_HOME:-$HOME/.local/share}
home=$data/rubycord
entry=$data/applications/rubycord.desktop
gate_entry=$data/applications/rubycord-gate-only.desktop

# Run from a terminal with no arguments: ask what to do.
if [ $# -eq 0 ] && [ -t 0 ] && [ -t 1 ]; then
   printf 'RubyCord\n  1) Inject\n  2) Un-Inject\n  q) Quit\n> '
   read -r choice
   case $choice in
      1) ;;
      2) set -- --uninstall ;;
      *) exit 0 ;;
   esac
fi

if [ "${1:-}" = --uninstall ]; then
   rm -rf "$home" "$entry" "$gate_entry"
   echo "RubyCord is uninstalled. Discord itself was not changed."
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
      echo "RubyCord can't be installed under $home: set XDG_DATA_HOME to a path without spaces, colons or quotes." >&2
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

# From when RubyCord was called Ruby.
rm -rf "$data/ruby" "$data/applications/ruby.desktop" "$data/applications/ruby-gate-only.desktop"
mkdir -p "$home" "$data/applications"
# The launcher preloads the loader installed beside it.
cat >"$home/rubycord" <<EOF
#!/bin/sh
home=$(q "$home")
while [ "\${1:-}" = --no-runtime ] || [ "\${1:-}" = --gate-only ]; do
   export RUBYCORD_GATE_ONLY=1
   shift
done
# A second Discord hands off to the running one and exits, so one running
# without RubyCord is restarted with it.
discord=$(q "$discord")
pids= rubycord=
for p in /proc/[0-9]*; do
   if [ "\$(readlink "\$p/exe" 2>/dev/null)" = "\$discord" ] ||
      tr '\\0' '\\n' <"\$p/environ" 2>/dev/null | grep -qxF "APPIMAGE=\$discord"; then
      pids="\$pids \${p#/proc/}"
      grep -q libphoenix "\$p/maps" 2>/dev/null && rubycord=1
   fi
done
if [ -n "\$pids" ] && [ -z "\$rubycord" ]; then
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
chmod +x "$home/rubycord"

exec_args=
[ -n "$no_runtime" ] && exec_args=" --no-runtime"
cat >"$entry" <<EOF
[Desktop Entry]
Type=Application
Name=RubyCord
Comment=Discord with RubyCord
Exec="$home/rubycord"$exec_args %U
Icon=discord
Terminal=false
Categories=Network;InstantMessaging;
MimeType=x-scheme-handler/discord;
EOF
# For when the plugin runtime crashes on a Discord build it doesn't know yet.
cat >"$gate_entry" <<EOF
[Desktop Entry]
Type=Application
Name=RubyCord (access gate only)
Comment=Discord past the access gate, without plugins
Exec="$home/rubycord" --no-runtime %U
Icon=discord
Terminal=false
Categories=Network;InstantMessaging;
EOF
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true

# The loader: appended to this file by CI, or named by RUBYCORD_LOADER.
rm -f "$home/libphoenix.so"
if [ -n "${RUBYCORD_LOADER:-}" ]; then
   cp "$RUBYCORD_LOADER" "$home/libphoenix.so"
else
   start=$(grep -a -n -m1 '^__PAYLOAD__$' "$0" | cut -d: -f1 || true)
   if [ -z "$start" ]; then
      echo "This copy has no loader attached. Download rubycord-installer-linux from the releases, or set RUBYCORD_LOADER." >&2
      exit 1
   fi
   tail -n +"$((start + 1))" "$0" >"$home/libphoenix.so"
fi
chmod +x "$home/libphoenix.so"
echo "RubyCord is installed. Open \"RubyCord\" from your app menu to start Discord with it."
echo "It runs Discord from $discord: if you move or replace that file, run this again."
exit 0
