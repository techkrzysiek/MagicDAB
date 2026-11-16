# Security

## Reporting a vulnerability

Please report it privately: on the repository page, Security > "Report a
vulnerability" (GitHub's private vulnerability reporting). Do not open a
public issue for it. Say which version it is (Settings > About, or
`magicdab --help`), how the receiver is reached (the tunnel, a reverse proxy,
a port of the local network) and what to send or do to see the problem.

The latest release is the one that gets fixes; installed copies and the
Docker image update to it.

## What the program defends

- **Listeners** are anyone who can reach the web interface. They may listen
  and, unless the admin locked the tuner, tune and scan. Nothing they send
  should take the receiver down, reach files of the computer, or make the
  server fetch from its own network.
- **The admin** (the admin password, the window of the desktop application,
  the Android app) sets the receiver up. The interface does not hand out
  files of the computer (a file named as the input must be an IQ recording,
  and its samples are not offered for download again), and it never shows the
  secrets it keeps (the tunnel's token, the Discord webhook, the password
  hashes).
- **What comes from the air or from broadcasters' servers** (labels, texts,
  slides, logos, SPI documents, RadioDNS and RadioVIS answers) is not
  trusted: it is shown as text or as a picture, never run, and addresses in
  it are fetched from the internet only.
- **Updates**: an installed copy installs a package only when the update
  manifest carries a signature of a key built into it (`SIGNING_KEYS` in
  `crates/server/src/update.rs`). The Docker image and the packages carry a
  build attestation (README, "Docker on Linux").

## Running a receiver

- Put the receiver behind HTTPS when the admin logs in over the internet:
  the tunnel does that, or a reverse proxy with a certificate. On a plain
  `http://` port the admin password travels readable.
- Give the admin password to people you would give the computer to. The
  tuning password is the one for trusted listeners.
- The data folder holds the settings and tokens: keep it readable by its
  user only (the program writes its files so).
