MagicDAB for Linux
==================

Start ./magicdab in this folder: the receiver starts and, on a desktop, the
interface opens in the browser (http://localhost:8080/). Other devices in
the network reach it at http://<this computer>:8080/. Ctrl+C stops it.

Settings, the station list and the log (magicdab.log) are kept in the
"data" folder next to the program (in ~/.local/share/magicdab when that
folder cannot be written to). At the first start a setup wizard asks
for the language, the station name, the location, the input device, an
admin password and the public tunnel; it can be skipped and opened again in
Settings > Webserver. Until a password is set, a random one is in the log.

RTL-SDR: the library comes with the program (librtlsdr.so.0, version 2,
which knows the RTL-SDR Blog V4) and needs libusb-1.0, present on nearly
every system. The DVB-T driver of the kernel is let go by itself. To use
the dongle without root, install the udev rules once and plug it in again:

    sudo cp 60-rtl-sdr.rules /etc/udev/rules.d/
    sudo udevadm control --reload-rules

FM tuner (FM-DX-Tuner / TEF668x headless USB, for DAB/FM linking): found by
itself; the serial port needs your user in the "dialout" group
(sudo usermod -aG dialout $USER, then log in again).

Updates: Settings > Internet > Updates checks for a new version and
installs it with one click (the folder must be writable by the user that
runs MagicDAB; the settings stay).

As a service (systemd), e.g. unpacked in /opt/MagicDAB for the user "radio",
in /etc/systemd/system/magicdab.service:

    [Unit]
    Description=MagicDAB
    After=network-online.target

    [Service]
    User=radio
    ExecStart=/opt/MagicDAB/magicdab --no-browser
    Restart=on-failure

    [Install]
    WantedBy=multi-user.target

then: sudo systemctl enable --now magicdab

Options: ./magicdab --listen 0.0.0.0:8080 --data-dir DIR --rtltcp HOST:PORT
         --channel 12D --no-browser --verbose
