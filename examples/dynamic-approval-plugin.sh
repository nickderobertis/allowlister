#!/usr/bin/env sh
# Example allowlister dynamic approval plugin.
# It reads a JSON request on stdin and returns a JSON verdict on stdout.
#
# The request is a tagged union on "subject": a shell command carries `command`
# and `fragments`; a tool call carries a `tool` object. This deliberately simple
# policy judges only shell commands — it allows those tagged "ticket=APPROVED",
# asks on "prod", and defers everything else. Any other subject (a tool call) is
# deferred untouched, so the naive substring match never runs against a tool's
# parameters or raw input.

request=$(cat)

# Only shell commands are judged here; defer every other subject.
case "$request" in
  *'"subject":"shell"'*) ;;
  *)
    printf '%s\n' '{"verdict":"defer","reason":"example plugin judges only shell commands"}'
    exit 0
    ;;
esac

case "$request" in
  *'ticket=APPROVED'*)
    printf '%s\n' '{"verdict":"allow","reason":"approved ticket tag present"}'
    ;;
  *'prod'*)
    printf '%s\n' '{"verdict":"ask","reason":"production command needs human approval"}'
    ;;
  *)
    printf '%s\n' '{"verdict":"defer","reason":"plugin has no matching approval"}'
    ;;
esac
