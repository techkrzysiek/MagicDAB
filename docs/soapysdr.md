# SoapySDR receivers (SDRplay RSP and others)

MagicDAB can take its samples from any receiver that has a
[SoapySDR](https://github.com/pothosware/SoapySDR) 0.8 module and can run at
2.048 MS/s, the sample rate of DAB. It loads `libSoapySDR` when the receiver
is chosen, so neither SoapySDR nor a module is part of the MagicDAB packages:
install them on the computer, then choose **Settings → Receiver → Input:
SoapySDR** (or start with `--soapy driver=sdrplay`).

The receiver field takes SoapySDR's device arguments, for example
`driver=sdrplay` or `driver=sdrplay,serial=…`; empty takes the first receiver
found. The **Found…** list shows what SoapySDR sees.

## Gain

The gains come from the driver and work as with an RTL-SDR: the software
AGC (default), the per-channel gain optimiser and the manual slider step
through them, 2 dB at a time. Hardware AGC hands the gain to the receiver.

SDRplay has gain reductions instead of gains (RFGR, the LNA state, and IFGR,
20 to 59 dB, lower is more gain). MagicDAB walks both together from the
lowest gain to the highest, and for an SDRplay it also:

- turns the DAB notch filter off: SoapySDRPlay3 turns it on, and it takes
  most of Band III away;
- keeps the FM notch on (strong FM stations nearby);
- lowers the setpoint of the receiver's own AGC (hardware AGC), which pumped
  on the peaks of DAB;
- sets the bias-T as in the settings.

## SDRplay on Linux

1. Install the SDRplay API 3.x from [sdrplay.com](https://www.sdrplay.com/)
   (it brings and starts the `sdrplay_apiService` daemon). Its licence does
   not let us ship it.
2. Install SoapySDR 0.8 (Debian, Ubuntu: `apt install libsoapysdr0.8
   soapysdr-tools`) and build
   [SoapySDRPlay3](https://github.com/pothosware/SoapySDRPlay3).
3. `SoapySDRUtil --find="driver=sdrplay"` should list the RSP.
4. The kernel's DVB modules for the RSP1 (`msi001`, `msi2500`) must not hold
   it: blacklist them.

## Docker

The MagicDAB image has no SoapySDR. An image of your own on top of it needs
SoapySDR, the module and, for an SDRplay, the API with its daemon running
before MagicDAB (`/dev/bus/usb` passed in, `device_cgroup_rules:
["c 189:* rmw"]`). Build it for yourself: the SDRplay API may not be passed on.

## Windows

Install SoapySDR with the module of the receiver, e.g. through
[PothosSDR](https://downloads.myriadrf.org/builds/PothosSDR/) or radioconda,
so that `SoapySDR.dll` is on the PATH; for an SDRplay also the SDRplay API.

## Limits

- 2.048 MS/s is needed exactly: a receiver that cannot do it (e.g. an Airspy
  R2, 2.5 or 10 MS/s) is refused with a message.
- We have no SDRplay ourselves: the SDRplay settings follow a setup reported
  to work with an RSP1A. The gains it shows are approximate (an LNA state
  counted as 6 dB), and other RSP models have not been tried.
