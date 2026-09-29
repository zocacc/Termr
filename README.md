# Termr

## Interactive session (MVP)

Press `Enter` on a host to connect. Unknown SSH host keys show the host,
algorithm, and fingerprint and require `y` to trust or `n`/`Esc` to reject.
Inside a session, normal text, Enter, Tab, Backspace, arrow keys, and common
Ctrl-key combinations are forwarded; terminal resize is propagated remotely.
Press `Esc` to close the session.

The MVP PTY is a byte-stream view intended for common line-oriented Linux and
network-device CLIs. It is not a full terminal emulator and has no local
scrollback, copy mode, mouse protocol, advanced escape-sequence interpretation,
or session transcript. Input and output remain ephemeral and are not written to
History, Collections, or logs.
