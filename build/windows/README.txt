MagicDAB for Windows
====================

Start magicdab.exe: MagicDAB opens in a window of its own (it needs the
Microsoft Edge WebView2 Runtime, part of Windows 10 and 11; without it the
browser is used). The receiver is also reachable from other devices in the
network: http://<this computer>:3222/

Windows Defender Firewall asks at the first start whether MagicDAB may be
reached from the network. If phones or other computers cannot reach it
(the question was closed, or the network is set as public), Settings >
Webserver > Windows firewall > Add an exception lets it in on every network
type (Windows asks for the administrator's permission).

Closing the window asks what to do: hide MagicDAB next to the clock (it
keeps playing and serving listeners in the network), close the window and
keep only the server running in the background, or quit. Settings > General
remembers the choice and can start MagicDAB as a server without a window (a
note shows for a few seconds, with a button that opens it). The icon next to
the clock shows the window, opens the interface in the browser, plays/stops
and quits. Starting magicdab.exe again shows the running window.

At the first start a setup wizard asks for the language, the station name,
the location, the input device, an admin password and the public tunnel; it
can be skipped and opened again in Settings > Webserver.

Settings, the station list and logs are kept in the "data" folder next to
the program (in %LOCALAPPDATA%\MagicDAB when that folder cannot be written
to, e.g. in Program Files). The window itself needs no admin password; for other devices
(tuning, settings) set one in Settings > Access - a random one is in the Log
after the first start.

Updates: Settings > Internet > Updates checks an update address and installs
a new version with one click (it restarts by itself; settings stay).
Unpacking a new zip over this folder works as well.

RTL-SDR: the dongle needs the WinUSB driver once. zadig.exe is in this
folder (it lists all devices, WinUSB chosen): pick the dongle, often
"Bulk-In, Interface (Interface 0)", and Install/Replace Driver. rtl_test.exe
checks it. Unpack the whole zip first: started from inside the zip the
program does not find rtlsdr.dll.

FM tuner (FM-DX-Tuner / TEF668x headless USB): off at first; once turned
on in Settings > FM and linking it is found on its COM port by itself (port
and sound card can be chosen there).

Options: magicdab.exe --listen 0.0.0.0:3222 --data-dir DIR --rtltcp HOST:PORT
         --hidden (start in the tray)
