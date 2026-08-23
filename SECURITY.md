# Security policy

## Supported versions

Security fixes target the latest released version. Older versions may not
receive patches.

## Update trust model

Release binaries are unsigned. The updater accepts only a newer stable SemVer
release from this repository, requires the exact versioned Inno Setup filename,
and checks its size and GitHub-provided SHA-256 digest before installation.
This detects download corruption but does not provide the publisher identity
guarantee of a code signature. Protecting the GitHub repository and release
workflow is therefore part of the update security boundary.

## Report a vulnerability

Use [GitHub private vulnerability reporting](https://github.com/2sBs/audio-switch/security/advisories/new)
instead of a public issue. Include the affected version, impact, reproduction
steps, and any suggested mitigation.

Remove audio device names, endpoint IDs, usernames, and local paths from logs or
settings before attaching them. Please allow time to confirm the report and
prepare a fix before publishing details.
