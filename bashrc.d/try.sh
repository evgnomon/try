# Load the try shell function when the binary is on PATH.
if command -v try >/dev/null 2>&1; then
  eval "$(command try init)"
fi
