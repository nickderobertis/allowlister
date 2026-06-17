#!/usr/bin/env sh
# Example allowlister dynamic approval plugin.
# It reads a JSON request on stdin and returns a JSON verdict on stdout.
# This deliberately simple policy allows only commands tagged with
# "ticket=APPROVED"; everything else defers to allowlister/the harness.

request=$(cat)
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
