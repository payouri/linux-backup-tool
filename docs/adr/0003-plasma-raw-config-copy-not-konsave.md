# Capture KDE Plasma by copying raw config files, not by depending on konsave

KDE Plasma layout is captured as raw config files via a Curated allowlist owned in the Manifest, and restored by untarring to `$HOME` then re-logging in — even though `konsave` is installed on the target machine and is the obvious prior art.

We don't depend on konsave because its capture is itself just a manifest-driven plain file copy, so our "untar to original paths" restore already reproduces what it does — adding a runtime dependency would buy nothing. We seed our allowlist from konsave's file set but add `~/.config/kdedefaults/` (which konsave misses) and make the heavy `~/.local/share/icons` (~226M) and wallpapers opt-out via Drill-down.

## Consequences

A reader who notices konsave on the box will wonder why we reimplement it. The answer is that there is nothing to reuse — konsave is a file-copy wrapper, and file-copy is what our archive already is. Our allowlist is a strict superset of konsave's coverage.
