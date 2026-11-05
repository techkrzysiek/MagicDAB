# Several multiplexes at once

One receiver decoded one multiplex and every listener heard a service of it;
tuning moved everybody. Listeners of instances without a tuner of their own
(EDI/ETI streams, IQ recordings mapped to channels) now choose their own
multiplex: steps 1 and 2 below are built (see "Built"). Two tuners (two
RTL-SDR sticks) in one server, step 3, are a plan.

## Before the slots

- `magicdab_dab::receiver::ReceiverHandle` is self-contained: its own thread,
  sample queue, ETI frame channel, command and event channels. Several can
  run side by side; nothing in the DSP needs to change.
- `radio.rs` holds one of them (`rx`) and everything that belongs to "the"
  multiplex: `ensemble`, `labels`, `services` (decoding state per service),
  `signal`, `eti_input`, `device`, slides, DLS, TII, the change log
  observation, the service list update, dynamic labels, RadioDNS, the follower.
- Clients choose a service (`select`) within the current multiplex; the hub
  sends audio and service data to the clients of a service.

## Slots

A **slot** is a receiver with its input and the state of the multiplex it
decodes. Slot 0 is what exists today: the tuner (or the default channel of an
instance without one); only the admin (or anyone when the tuner is not
locked) moves it, as now. Further slots are made on demand:

| input of the channel             | further slots                         | cost per slot            |
|----------------------------------|---------------------------------------|--------------------------|
| ETI/EDI stream (`edi+tcp://...`) | yes, one per channel                  | FIC + listened services  |
| IQ recording (`file://...`)      | yes, limited (setting, default 2)     | OFDM demodulation, ~1/4 core here |
| RTL-SDR / rtl_tcp                | only with a second tuner (dual tuner) | the stick                |

A listener who picks a channel that is not on slot 0 is moved to the slot of
that channel (made if needed) instead of retuning slot 0. A slot without
listeners is closed after a minute (no flapping when someone hops around).

A fourth kind of slot plays a **DX recording** (`dxrec.rs`, `radio/dxrec.rs`):
once an admin switches it on, a multiplex heard on a channel for the first
time is recorded from slot 0 as ETI(NI) frames (the FIC and every sub-channel
decoded, `etirec.rs` in the DAB crate; about 15 MB a minute, a minute unless
set longer) into `data/dxrec`, and an admin plays such a file, or an .eti or
.edi file of its own, back on a slot of its own (`Kind::Playback`; a click on
a station of the list plays that one): the file
is fed to the receiver's ETI channel at the pace of the air, in a loop, nobody
else is moved, and the slot closes a few seconds after the admin leaves it. A
playback is not a channel listeners join and takes no place of theirs.

### State split

`struct Slot { id, channel, frequency, rx: ReceiverHandle, input: SlotInput,
ensemble, labels, services, wants, signal, slide_cache keys, data_progress,
tuned_at, ... }` and `Radio { slots: Vec<Slot>, ... }` with the shared parts
staying in `Radio` (settings, hub, service list, change tracker, gallery,
SPI store, TxDb, tunnel, FM tuner, follower).

Only slot 0 feeds the things that are about "the" receiver: the change log
(`observe_changes`), the service list update, dynamic label learning,
TII, the band and DX scans, RadioDNS, the DAB/FM follower, the public
snapshot (`/api`, link previews). Side slots give listening only: the
ensemble, signal, audio, DLS/DL+, slides and EPG of their services.

### Events

The radio thread selects on slot 0's events and the commands today. With
slots: each slot's receiver gets a forwarding thread that wraps its events as
`Command::Slot(id, RxEvent)` into the command channel (or a
`crossbeam_channel::Select` over all slots). `on_rx(ev)` becomes
`on_rx(slot, ev)`; most handlers only touch `self.slots[slot]`.

### Clients

`hub::Client` gets `slot: usize`. Messages that are about a multiplex
(`ensemble`, `signal`, `state`, `tii`, `dataProgress`, slides, DLS, audio,
`select` with reason `tuned`) go to the clients of that slot
(`hub.broadcast(m, |c| c.slot == id)`); those about the server (`hello`,
service list, change log, chat, settings) to all. The interface needs
almost nothing: it shows what it is sent. The state message gets
`"slot"` and `"slotOwner"` (whether this listener moved the tuner), so the UI
can say "you listen to 10D, the tuner is on 5B".

`tune` from a listener: slot 0 when allowed as today (tuner not locked,
admin, or tuning password); otherwise, and always for channels with an
ETI/EDI or file input when the setting "listeners choose their own multiplex"
is on, the listener's own slot. Allowed channels (access control) apply.

## Dual tuner

The same slots with a second physical input: settings get `devices: [..]`
(today `device`); slot 1 is made at start for the second stick and kept.
Listeners choose "tuner 1 / tuner 2" (or simply the channel, and the server
takes a free tuner when the tuner lock allows it). Scans and DX scans run on
one chosen tuner. Both sticks on one USB hub need enough current; two
RTL-SDR at 2.048 MS/s are fine on USB 2.

## Steps

1. Slots for ETI/EDI-mapped channels (cheap, no OFDM), side slots listening
   only; setting "listeners choose their own multiplex" (on by default for
   instances without a tuner).
2. File recordings as slots, with the CPU limit.
3. Dual tuner (`devices`), tuner choice in the interface.

Tests: two clients on a test server, one on 10C (EDI), one on 8C (recording), each
hearing its own service and seeing its own ensemble; a third client tuning
slot 0 does not move them; slots close after the listeners leave.

## Built (steps 1 and 2)

`crates/server/src/radio/slots.rs`; settings `listenerMultiplex` (on by
default) and `listenerMultiplexLimit` (side slots at once, default 4) in
Settings > Receiver, card of the channel maps.

- A listener (not the admin) tuning to a mapped channel, by the channel
  selector, previous/next or a station of the station list, joins the main
  receiver when it is on that channel, else the side slot of the channel
  (made when needed). The tuner lock is for the main receiver only; allowed
  channels (Settings > Access) still apply. Unmapped channels and the admin
  move the main receiver as before; an admin who tunes leaves a side slot.
- One side slot per channel, shared by its listeners. Recordings: at most
  two side slots at once (each one demodulates OFDM). When there is no room
  the listener stays where it is and gets a notice; a slot nobody has
  listened to for 10 s makes room for a new one. A slot closes a minute
  after its last listener left. When the main receiver tunes to the channel of a side slot
  (not during a scan), its listeners move to the main receiver with their
  stations and the side slot closes.
- `hub::Client::slot` says where a client listens; messages about a
  multiplex go to the clients of its slot, audio by slot and service (an
  `AudioHub` per slot). The state message carries `slot` and
  `mainChannel`; the interface notes "you listen to 8C · the main receiver
  is on 10C" (service list and Stations view). After a reconnect the page
  asks for its own multiplex again (`rejoin`).
- Side slots share the SPI store (logos, programme guides) and the last
  slides of the stations per multiplex; they do not learn the FIG cache.
- Measured on the 2-core test host: a recording slot takes about 26 % of a
  core with background PAD (DLS and slides of all stations), 8-12 % without;
  a stream slot about 1-2 %.
