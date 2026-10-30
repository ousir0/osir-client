# Sourced inside the generated subshell; all settings stay in this child.
export PATH="$osir_claude_prefix/bin:$PATH"
export npm_config_allow_scripts=@anthropic-ai/claude-code npm_config_ignore_scripts=false npm_config_include=optional
export npm_config_prefix="$osir_claude_prefix"
osir_claude_npm="$osir_claude_prefix/bin/npm"
if [ ! -x "$osir_claude_npm" ]; then
  osir_claude_npm="$(command -v npm)" || { printf '%s\n' 'npm: command not found' >&2; exit 127; }
fi
osir_claude_install() {
  "$osir_claude_npm" i -g "$osir_claude_package" --prefix "$osir_claude_prefix" --ignore-scripts=false --include=optional --allow-scripts=@anthropic-ai/claude-code
}
osir_claude_verify() {
  osir_claude_probe=$("$osir_claude_launcher" --version 2>&1) || return 1
  case "$osir_claude_probe" in
    *"Run the postinstall manually"*|*"Could not find native binary package"*|*"Missing optional dependency"*|*"claude native binary not installed"*|*"TOOL_INSTALL_NATIVE_MISSING"*) return 1;;
  esac
  [[ "$osir_claude_probe" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+([[:space:]]|[-+.]|$) ]]
}
osir_claude_install || exit $?
if osir_claude_verify; then exit 0; fi
osir_claude_repair=false
case "$osir_claude_probe" in
  *"Run the postinstall manually"*|*"Could not find native binary package"*|*"Missing optional dependency"*|*"claude native binary not installed"*|*"TOOL_INSTALL_NATIVE_MISSING"*) osir_claude_repair=true;;
esac
if [ ! -e "$osir_claude_launcher" ]; then osir_claude_repair=true; fi
if [ "$osir_claude_repair" = true ]; then
  "$osir_claude_npm" uninstall -g @anthropic-ai/claude-code --prefix "$osir_claude_prefix" || exit $?
  osir_claude_install || exit $?
  if osir_claude_verify; then exit 0; fi
fi
if [ ! -e "$osir_claude_launcher" ]; then
  printf '%s\n' TOOL_INSTALL_NATIVE_MISSING >&2
  exit 1
fi
case "$osir_claude_probe" in
  *"Run the postinstall manually"*|*"Could not find native binary package"*|*"Missing optional dependency"*|*"claude native binary not installed"*|*"TOOL_INSTALL_NATIVE_MISSING"*) printf '%s\n' TOOL_INSTALL_NATIVE_MISSING >&2;;
  *) printf '%s\n' TOOL_INSTALL_FAILED >&2;;
esac
exit 1
