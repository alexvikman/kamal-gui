# Security

Please report vulnerabilities privately through GitHub:
**Security → Report a vulnerability** on this repository
(private vulnerability reporting). Don't open a public issue.

Kamal GUI runs kamal, git, ssh, op and gh on your machine, reads deploy
configs from the repositories you add, and handles 1Password secrets in
memory. Reports about anything that could make it run unintended commands,
leak secrets, or act on the wrong servers are especially welcome.

Release artifacts come with signed build provenance; verify with
`gh attestation verify <file> -R alexvikman/kamal-gui`.
