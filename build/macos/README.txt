MagicDAB for macOS
==================

One program for Apple silicon and Intel Macs (macOS 11 or newer).

It is not signed by Apple, so macOS refuses it at first. Once, in Terminal:

    xattr -dr com.apple.quarantine /path/to/MagicDAB

(or right-click magicdab, Open, Open). Then start magicdab by double-click
or ./magicdab in Terminal: the receiver starts and the interface opens in
the browser (http://localhost:3222/). Other devices in the network reach it
at http://<this Mac>:3222/. Closing the Terminal window stops it.

Settings, the station list and the log (magicdab.log) are kept in the
"data" folder next to the program (in ~/Library/Application Support/MagicDAB
when that folder cannot be written to). At the first start a setup wizard asks
for the language, the station name, the location, the input device, an
admin password and the public tunnel; it can be skipped and opened again in
Settings > Webserver. Until a password is set, a random one is in the log.

RTL-SDR: the library (librtlsdr, version 2, which knows the RTL-SDR Blog
V4) and libusb come with the program; no driver is needed.

The FM tuner for DAB/FM linking is not supported on macOS yet.

Updates: Settings > Internet > Updates checks for a new version and
installs it with one click (the settings stay).

Options: ./magicdab --listen 0.0.0.0:3222 --data-dir DIR --rtltcp HOST:PORT
         --channel 12D --no-browser --verbose
