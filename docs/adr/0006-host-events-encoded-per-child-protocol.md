# 0006. Host key events encoded per the child's protocol

Status: accepted, 2026-10-08

## Context

Two input designs were considered. Raw passthrough copies the host's
bytes to the child and intercepts one prefix byte. Event encoding reads
crossterm events from the host and encodes them for the child according
to the protocol the child asked for. Raw passthrough fails as soon as the
child enables the kitty protocol or modifyOtherKeys: the prefix stops being
a byte, the host and the child may disagree on the protocol, and the side
panes still need decoded keys. Resize, paste and focus events also come
from crossterm, not from the byte stream.

## Decision

`term` is the terminal the agent talks to. The host is read as crossterm
events, with the kitty disambiguate flag pushed on hosts that support it.
Keys are encoded for the child in legacy xterm, modifyOtherKeys level 2 or
kitty form, selected by the modes the emulator recorded from the child's
own requests; cursor keys follow DECCKM and pastes are bracketed when the
child asked for it. The prefix `Ctrl+Q` is recognised as an event, so it
works under every protocol.

Evidence from the spike: with the host terminal variables removed, Claude
Code 2.1.295 probes `CSI ? u`, `CSI > 0 q` and `CSI c`; after the
emulator's `CSI ? 0 u` reply it enables kitty flags 5 and modifyOtherKeys
2, and Shift+Enter reaches it as `CSI 13;2u` and inserts a newline.

## Consequences

- `src/keys.rs` is the contract: one table per protocol, tested against
  the kitty and xterm specifications.
- Kitty event types, alternate keys and associated text are not reported;
  no agent has needed them.
- Ctrl+Z makes the agent suspend itself inside the pane; M2 stops
  forwarding it and resumes a stopped child.
- A host without the kitty protocol cannot distinguish Shift+Enter from
  Enter; the encoder can only pass on what the host reports.
