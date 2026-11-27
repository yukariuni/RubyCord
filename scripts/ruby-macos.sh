#!/bin/sh
# Installs Ruby for Desktop Phoenix on macOS: a Ruby app in
# /Applications (~/Applications for accounts that can't write there) that starts a re-signed copy of Discord Phoenix with the
# loader inserted. Discord's own app stays untouched, so its updates never undo
# Ruby, and the copy is rebuilt whenever Discord changes.
#
#   RUBY_SITE=http://127.0.0.1:8000 sh scripts/ruby-macos.sh
#   RUBY_SITE=http://127.0.0.1:8000 sh scripts/ruby-macos.sh "/path/to/Discord Phoenix.app"
#   RUBY_SITE=http://127.0.0.1:8000 sh scripts/ruby-macos.sh --uninstall
#
# RUBY_SITE points it at a local server for testing.
#
# Phoenix is signed with the hardened runtime, which ignores
# DYLD_INSERT_LIBRARIES and refuses libraries Discord did not sign. The copy is
# signed ad hoc without it. Its restricted entitlements (keychain and app
# groups) can't come along, so the copy may ask you to log in once.
set -eu

site=${RUBY_SITE:-}
home="$HOME/Library/Application Support/Ruby"
# /Applications is writable by admin accounts only.
if [ -w /Applications ]; then
   app=/Applications/Ruby.app
else
   app="$HOME/Applications/Ruby.app"
fi
bundle_id=com.discord.DesktopGpui

if [ "${1:-}" = --uninstall ]; then
   rm -rf "$home" /Applications/Ruby.app "$HOME/Applications/Ruby.app"
   echo "Ruby is uninstalled. Discord itself was not changed."
   exit 0
fi

if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
   echo "Discord Phoenix for macOS runs on Apple silicon only." >&2
   exit 1
fi

# What the site serves is loaded into Discord, so only https, or plain http to
# this machine for testing.
case $site in
   https://*) proto==https ;;
   http://127.0.0.1 | http://127.0.0.1[:/]* | http://localhost | http://localhost[:/]*) proto==http ;;
   *)
      echo "RUBY_SITE must be https:// or a localhost server, not $site" >&2
      exit 1
      ;;
esac

# DYLD_INSERT_LIBRARIES splits on colons, and the launcher quotes the rest.
case $home in
   *[:\"\$\`\\\']*)
      echo "Ruby can't be installed under $home: it has a colon or quote." >&2
      exit 1
      ;;
esac

is_phoenix() {
   [ -f "$1/Contents/Info.plist" ] &&
      [ "$(/usr/bin/plutil -extract CFBundleIdentifier raw "$1/Contents/Info.plist" 2>/dev/null)" = $bundle_id ]
}

script="curl -fsSL $site/plugins/ruby-macos.sh | sh -s --"
discord=${1:-}
if [ -n "$discord" ] && ! is_phoenix "$discord"; then
   echo "$discord is not Discord Phoenix. Pass the path of Discord Phoenix's app:" >&2
   echo "  $script \"/Applications/Discord Phoenix.app\"" >&2
   exit 1
fi
if [ -z "$discord" ]; then
   for candidate in "/Applications/Discord Phoenix.app" "$HOME/Applications/Discord Phoenix.app"; do
      if is_phoenix "$candidate"; then
         discord=$candidate
         break
      fi
   done
fi
if [ -z "$discord" ]; then
   # Spotlight finds it wherever it was moved or renamed.
   discord=$(/usr/bin/mdfind "kMDItemCFBundleIdentifier == '$bundle_id'" 2>/dev/null |
      while read -r found; do
         # Not Ruby's own copy.
         [ "${found#"$home"/}" = "$found" ] || continue
         if is_phoenix "$found"; then
            echo "$found"
            break
         fi
      done)
fi
if [ -z "$discord" ]; then
   cat >&2 <<EOF
Couldn't find Discord Phoenix in /Applications, ~/Applications or Spotlight.

Install Discord Phoenix, then run this again, with its path if it lives
somewhere else:
  $script "/path/to/Discord Phoenix.app"
EOF
   exit 1
fi
discord=$(cd "$discord" && pwd -P)
echo "Using Discord Phoenix at $discord"

# Single-quotes a value for the generated launcher.
q() { printf "'%s'" "$(printf %s "$1" | sed "s/'/'\\\\''/g")"; }

# An earlier install may have used the other folder.
for other in /Applications/Ruby.app "$HOME/Applications/Ruby.app"; do
   [ "$other" = "$app" ] || rm -rf "$other"
done
mkdir -p "$home" "$app/Contents/MacOS" "$app/Contents/Resources"

# The launcher brings the loader up to the site's version and the copy up to
# Discord's before each start, and keeps what it has when offline. Each run
# downloads and copies to its own name, so two starts at once never use a
# half-written file.
cat >"$app/Contents/MacOS/ruby" <<EOF
#!/bin/sh
home=$(q "$home")
site=$(q "$site")
discord=$(q "$discord")
copy="\$home/Ruby.app"
# Bumped when the copy's branding changes, so existing copies are rebuilt.
brand=3
fetch() { /usr/bin/curl -fsSL --proto $proto --proto-redir =https "\$@" 2>/dev/null; }
fail() {
   /usr/bin/osascript -e 'on run argv' -e 'display alert "Ruby" message (item 1 of argv)' \\
      -e 'end run' -- "\$1" >/dev/null 2>&1
   echo "\$1" >&2
   exit 1
}

version=\$(fetch --max-time 5 "\$site/version.json" |
   sed -n 's/.*"loader": *"\([^"]*\)".*/\1/p')
if [ -n "\$version" ] && [ "\$version" != "\$(cat "\$home/version" 2>/dev/null)" ]; then
   tmp=\$(mktemp "\$home/libphoenix.dylib.XXXXXX")
   if fetch --max-time 60 -o "\$tmp" "\$site/plugins/libphoenix-macos-arm64.dylib"; then
      mv "\$tmp" "\$home/libphoenix.dylib"
      echo "\$version" >"\$home/version"
   else
      rm -f "\$tmp"
   fi
fi
[ "\${1:-}" = --update ] && exit 0
[ -d "\$discord" ] || fail "Discord Phoenix is gone from \$discord. Run the Ruby installer again."

# Rebuilt when Discord's executable changed, or when the copy lost its ad hoc
# signature, which Discord's updater would put back. An update the updater
# installed into the copy is newer than Discord Phoenix, so it is kept.
version() { /usr/bin/plutil -extract CFBundleVersion raw "\$1/Contents/Info.plist" 2>/dev/null; }
stamp="\$(/usr/bin/stat -f '%m %z' "\$discord/Contents/MacOS/Discord") brand \$brand"
if [ "\$stamp" != "\$(cat "\$home/stamp" 2>/dev/null)" ] ||
   ! /usr/bin/codesign -dv "\$copy" 2>&1 | grep -q 'Signature=adhoc'; then
   source=\$discord
   if [ -d "\$copy" ] && [ "\$(version "\$copy")" -gt "\$(version "\$discord")" ] 2>/dev/null; then
      source=\$copy
   fi
   tmp=\$(mktemp -d "\$home/Ruby.XXXXXX")
   new="\$tmp/Ruby.app"
   # A clone on APFS, so the copy takes no space until Discord updates.
   if ! /bin/cp -cR "\$source" "\$new" 2>/dev/null; then
      /usr/bin/ditto "\$source" "\$new" || { rm -rf "\$tmp"; fail "Couldn't copy Discord Phoenix."; }
   fi
   /usr/bin/xattr -cr "\$new"
   # Named and drawn as Ruby in the Dock, the app switcher and the menu
   # bar. The bundle id stays Discord's, which its data and login go by. The
   # asset catalog's icon outranks CFBundleIconFile, so its name goes.
   plist="\$new/Contents/Info.plist"
   /usr/bin/plutil -replace CFBundleName -string Ruby "\$plist"
   /usr/bin/plutil -replace CFBundleDisplayName -string Ruby "\$plist"
   # Discord relaunches itself with a bare \`open -n\`, so the copy loads
   # Ruby from its own Info.plist.
   /usr/bin/plutil -replace LSEnvironment.DYLD_INSERT_LIBRARIES -string "\$home/libphoenix.dylib" "\$plist"
   # Where the loader points Discord's relaunch after an update, so the new
   # build starts through this launcher too.
   /usr/bin/plutil -replace LSEnvironment.RUBY -string $(q "$app") "\$plist"
   if [ -f "\$home/Ruby.icns" ]; then
      cp "\$home/Ruby.icns" "\$new/Contents/Resources/Ruby.icns"
      /usr/bin/plutil -remove CFBundleIconName "\$plist" 2>/dev/null || true
      /usr/bin/plutil -replace CFBundleIconFile -string Ruby "\$plist"
   fi
   # Ad hoc, without the hardened runtime or Discord's entitlements; the
   # helpers and frameworks inside keep Discord's own signatures.
   if ! /usr/bin/codesign --force --sign - "\$new" >/dev/null 2>&1; then
      rm -rf "\$tmp"
      fail "Couldn't sign the copy of Discord Phoenix."
   fi
   rm -rf "\$copy" "\$home/Discord.app"
   mv "\$new" "\$copy"
   rmdir "\$tmp"
   echo "\$stamp" >"\$home/stamp"
   # The Dock and Finder cache icons by path until told the bundle changed.
   /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "\$copy" 2>/dev/null || true
fi

[ -f "\$home/libphoenix.dylib" ] ||
   fail "Ruby hasn't been downloaded yet. Check your connection and open Ruby again."

# A second Discord hands off to the running one and exits, so one running
# without Ruby, Discord Phoenix itself or the copy, is quit first. One that
# has Ruby is brought forward by open.
pids= ruby=
for pid in \$(/bin/ps -axo pid=,comm= | /usr/bin/awk \
   -v phoenix="\$discord/Contents/MacOS/Discord" -v copy="\$copy/Contents/MacOS/Discord" \
   '{ pid = \$1; sub(/^ *[0-9]+ /, ""); if (\$0 == phoenix || \$0 == copy) print pid }'); do
   pids="\$pids \$pid"
   /usr/sbin/lsof -p "\$pid" 2>/dev/null | grep -q libphoenix && ruby=1
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
exec /usr/bin/open -a "\$copy" --args "\$@"
EOF
chmod +x "$app/Contents/MacOS/ruby"

# Ruby's mark, the dashboard's sun, drawn with AppKit so no image ships.
make_icon() {
   work=$(mktemp -d)
   /usr/bin/osascript -l JavaScript - "$work/icon.png" >/dev/null 2>&1 <<'JS' || { rm -rf "$work"; return 1; }
function run(argv) {
   ObjC.import('AppKit');
   const size = 1024, center = size / 2, unit = (size / 32) * 0.86;
   const rep = $.NSBitmapImageRep.alloc.initWithBitmapDataPlanesPixelsWidePixelsHighBitsPerSampleSamplesPerPixelHasAlphaIsPlanarColorSpaceNameBytesPerRowBitsPerPixel(
      null, size, size, 8, 4, true, false, $.NSDeviceRGBColorSpace, 0, 0);
   $.NSGraphicsContext.saveGraphicsState;
   $.NSGraphicsContext.setCurrentContext($.NSGraphicsContext.graphicsContextWithBitmapImageRep(rep));
   [[14, 0x1e1f22], [12, 0xf59e0b], [5, 0xfff3b0]].forEach(([r, rgb]) => {
      $.NSColor.colorWithSRGBRedGreenBlueAlpha((rgb >> 16 & 255) / 255, (rgb >> 8 & 255) / 255, (rgb & 255) / 255, 1).setFill;
      $.NSBezierPath.bezierPathWithOvalInRect($.NSMakeRect(center - r * unit, center - r * unit, 2 * r * unit, 2 * r * unit)).fill;
   });
   $.NSGraphicsContext.restoreGraphicsState;
   rep.representationUsingTypeProperties($.NSBitmapImageFileTypePNG, $()).writeToFileAtomically(argv[0], true);
}
JS
   mkdir "$work/Ruby.iconset"
   for size in 16 32 128 256 512; do
      /usr/bin/sips -z $size $size "$work/icon.png" --out "$work/Ruby.iconset/icon_${size}x${size}.png" >/dev/null &&
         /usr/bin/sips -z $((size * 2)) $((size * 2)) "$work/icon.png" \
            --out "$work/Ruby.iconset/icon_${size}x${size}@2x.png" >/dev/null || { rm -rf "$work"; return 1; }
   done
   /usr/bin/iconutil -c icns "$work/Ruby.iconset" -o "$1"
   status=$?
   rm -rf "$work"
   return $status
}
if make_icon "$home/Ruby.icns"; then
   cp "$home/Ruby.icns" "$app/Contents/Resources/Ruby.icns"
else
   echo "Couldn't draw Ruby's icon; it keeps a plain one." >&2
fi
cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
   <key>CFBundleExecutable</key><string>ruby</string>
   <key>CFBundleIdentifier</key><string>tools.ruby.launcher</string>
   <key>CFBundleName</key><string>Ruby</string>
   <key>CFBundleDisplayName</key><string>Ruby</string>
   <key>CFBundleIconFile</key><string>Ruby</string>
   <key>CFBundlePackageType</key><string>APPL</string>
   <key>CFBundleShortVersionString</key><string>1.0</string>
   <key>LSMinimumSystemVersion</key><string>13.0</string>
   <key>LSUIElement</key><true/>
</dict>
</plist>
EOF
/usr/bin/codesign --force --sign - "$app" >/dev/null 2>&1 || true
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$app" 2>/dev/null || true

# First download now, so a missing network shows up here rather than as a
# Discord without Ruby.
echo "Downloading Ruby..."
rm -f "$home/version"
"$app/Contents/MacOS/ruby" --update
if [ -f "$home/libphoenix.dylib" ]; then
   echo "Ruby is installed at $app. Quit Discord, then open Ruby from there or Spotlight."
else
   echo "Installed, but downloading Ruby failed. It will retry each time you open it." >&2
fi
echo "It runs a copy of Discord Phoenix from $discord: if you move that app, run this again."
