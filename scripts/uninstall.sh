#!/bin/sh
# Wetware uninstaller
# Removes ~/.ww and prints PATH cleanup guidance.
set -eu

WW_HOME="${HOME}/.ww"

if [ ! -d "$WW_HOME" ]; then
  echo "Nothing to uninstall: ${WW_HOME} does not exist."
  exit 0
fi

echo "This will remove ${WW_HOME} and all its contents:"
echo "  - ${WW_HOME}/bin/ww        (binary)"
echo "  - ${WW_HOME}/lib/std/      (standard library)"
echo "  - ${WW_HOME}/etc/          (config templates)"
echo ""
printf "Continue? [y/N] "
read -r REPLY
case "$REPLY" in
  y|Y|yes|YES) ;;
  *)
    echo "Aborted."
    exit 0
    ;;
esac

rm -rf "$WW_HOME"
echo "Removed ${WW_HOME}"

echo ""
echo "Clean up your PATH by removing the ww entry from your shell config:"
echo ""
echo "  # bash: edit ~/.bashrc and remove:"
echo "  export PATH=\"${WW_HOME}/bin:\$PATH\""
echo ""
echo "  # zsh: edit ~/.zshrc and remove:"
echo "  export PATH=\"${WW_HOME}/bin:\$PATH\""
echo ""
echo "  # fish: run:"
echo "  set -e fish_user_paths[(contains -i ${WW_HOME}/bin \$fish_user_paths)]"
echo ""
echo "Wetware has been uninstalled."
