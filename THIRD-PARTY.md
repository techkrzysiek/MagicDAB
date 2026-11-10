# Third-party components

MagicDAB itself is licensed under the GNU AGPL v3 or later (see `LICENSE` and
`LICENSE-EXCEPTION.txt`). It is built with, and its packages contain, the
components below under their own licenses. All of them allow their use in a
program licensed under the AGPL.

## Native libraries

| Component | Version | License | Where | Source |
|---|---|---|---|---|
| faad2 (AAC decoder, DAB+) | 2.11.2 | GPL-2.0-or-later | linked into magicdab.exe; Linux: system library | https://github.com/knik0/faad2/archive/refs/tags/2.11.2.tar.gz |
| libopus (Opus encoder) | 1.5.2 | BSD-3-Clause | linked into magicdab.exe; Linux: system library | https://downloads.xiph.org/releases/opus/opus-1.5.2.tar.gz |
| minimp3 (MPEG Layer II decoder) | - | CC0-1.0 | built in (`third_party/minimp3`) | this repository |
| librdsparser (RDS decoder) by Konrad Kosmatka | see `third_party/librdsparser/VERSION` | LGPL-2.1-or-later | built in (`third_party/librdsparser`) | this repository |
| librtlsdr (rtl-sdr-blog: rtlsdr.dll, rtl_test.exe) | V1.4.0 | GPL-2.0-or-later | Windows package; Linux: system library | https://github.com/rtlsdrblog/rtl-sdr-blog/releases/tag/V1.4.0 |
| libusb (inside rtlsdr.dll) | - | LGPL-2.1-or-later | Windows package | https://github.com/libusb/libusb |
| pthreads-win32 (pthreadVC2.dll) | 2 | LGPL-2.1-or-later | Windows package | https://sourceware.org/pthreads-win32/ |
| Microsoft Visual C++ 2010 runtime (msvcr100.dll) | - | Microsoft redistributable | Windows package (used by rtlsdr.dll) | - |
| Microsoft Edge WebView2 Loader (WebView2Loader.dll) | - | Microsoft WebView2 SDK | Windows package (the window) | - |

The GPL and LGPL components are distributed unmodified; their source is
available at the addresses above (release packages of MagicDAB should also
carry these source archives).

## Ported code

- The DTR-100 / C100 dongle driver (`crates/sdr/src/dtr100.rs`) is a port of
  the register-level driver of DABplay+ by jvspier, MIT licence
  (https://github.com/jvspier/dabplayplus, `driver-dtr100`), including its
  register tables:

  > MIT License
  >
  > Copyright (c) 2026 jvspier
  >
  > Permission is hereby granted, free of charge, to any person obtaining a copy
  > of this software and associated documentation files (the "Software"), to deal
  > in the Software without restriction, including without limitation the rights
  > to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
  > copies of the Software, and to permit persons to whom the Software is
  > furnished to do so, subject to the following conditions:
  >
  > The above copyright notice and this permission notice shall be included in all
  > copies or substantial portions of the Software.
  >
  > THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
  > IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
  > FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
  > AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
  > LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
  > OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
  > SOFTWARE.

- libusb-1.0 (LGPL-2.1-or-later) is loaded at run time for the DTR-100 when
  the system has it; it is not part of the Linux packages.

## Data

- The names of the TV-Anytime content genres (`crates/dab/src/genres_table.rs`,
  `web/src/genres_table.ts`, made by `build/genres.py`) are those of the
  ContentCS classification scheme (`urn:tva:metadata:cs:ContentCS:2011`) of
  ETSI TS 102 822-3-1, as published in XML by the DVB Project and used by
  DVB-I tools (https://github.com/paulhiggs/dvb-i-tools, `res/tva/ContentCS.xml`).

## Run time

- frp client (frpc), Apache-2.0, downloaded from https://github.com/fatedier/frp
  when the tunnel is used (not part of the packages).
- Map data © OpenStreetMap contributors (ODbL), map tiles loaded by the browser.

## Web interface (JavaScript, in the bundle)

- **MIT**: @wasm-audio-decoders/common 9.0.7, flag-icons 7.6.0, js-tokens 4.0.0, loose-envify 1.4.0, opus-decoder 0.7.12, react 18.3.1, react-dom 18.3.1, scheduler 0.23.2, simple-yenc 1.0.4, zustand 5.0.16
- **Apache-2.0**: @eshaz/web-worker 1.2.2
- **BSD-2-Clause**: leaflet 1.9.4
- **ISC**: lucide-react 1.62.0

The Opus decoder (opus-decoder) contains libopus compiled to WebAssembly
(BSD-3-Clause).

## Rust crates (compiled into the program)

- **MIT OR Apache-2.0** (189): android_system_properties 0.1.6, base64 0.22.1, base64 0.23.1, bitflags 2.13.2, block-buffer 0.10.4, block-buffer 0.12.1, bumpalo 3.20.3, cc 1.10.0, cfg-expr 0.15.8, cfg-if 1.0.5, chrono 0.4.45, cookie 0.18.2, core-foundation 0.10.1, core-foundation-sys 0.8.7, core-graphics 0.25.0, core-graphics-types 0.2.0, cpufeatures 0.2.17, cpufeatures 0.3.1, crc32fast 1.5.3, crossbeam-channel 0.5.17, crossbeam-utils 0.8.23, crypto-common 0.1.7, crypto-common 0.2.2, deranged 0.5.9, digest 0.10.7, digest 0.11.3, dirs 7.0.0, dirs-sys 0.5.0, displaydoc 0.2.7, dtoa 1.0.11, errno 0.3.14, fdeflate 0.3.7, field-offset 0.3.6, find-msvc-tools 0.1.15, flate2 1.1.10, form_urlencoded 1.2.2, futures-channel 0.3.34, futures-core 0.3.34, futures-executor 0.3.34, futures-io 0.3.34, futures-macro 0.3.34, futures-sink 0.3.34, futures-task 0.3.34, futures-util 0.3.34, getrandom 0.2.17, getrandom 0.3.4, hashbrown 0.17.1, heck 0.4.1, heck 0.5.0, html5ever 0.39.0, http 1.5.0, httparse 1.10.1, httpdate 1.0.3, hybrid-array 0.4.15, iana-time-zone 0.1.65, iana-time-zone-haiku 0.1.2, idna 1.1.0, itoa 1.0.18, jni-sys 0.3.1, jni-sys 0.4.1, jni-sys-macros 0.4.1, js-sys 0.3.106, keyboard-types 0.8.3, libc 0.2.191, lock_api 0.4.14, log 0.4.34, markup5ever 0.39.0, mime 0.3.17, ndk 0.9.0, ndk-context 0.1.1, ndk-sys 0.6.0+11769913, num-complex 0.4.6, num-conv 0.2.2, num-integer 0.1.47, num-traits 0.2.19, once_cell 1.21.4, parking_lot 0.12.5, parking_lot_core 0.9.12, percent-encoding 2.3.2, pkg-config 0.3.34, png 0.18.1, powerfmt 0.2.1, ppv-lite86 0.2.21, primal-check 0.3.4, proc-macro-crate 1.3.1, proc-macro-crate 2.0.2, proc-macro-crate 3.5.0, proc-macro-error 1.0.4, proc-macro-error-attr 1.0.4, proc-macro2 1.0.107, quote 1.0.47, rand 0.8.8, rand 0.9.5, rand_chacha 0.3.1, rand_chacha 0.9.0, rand_core 0.6.4, rand_core 0.9.5, rustc_version 0.4.1, rustfft 6.4.1, rustls-pki-types 1.15.1, rustversion 1.0.23, scopeguard 1.2.0, semver 1.0.28, serde 1.0.229, serde_core 1.0.229, serde_derive 1.0.229, serde_json 1.0.154, serde_path_to_error 0.1.20, serde_spanned 0.6.9, servo_arc 0.4.3, sha1 0.10.7, sha2 0.10.9, sha2 0.11.1, shlex 2.0.1, signal-hook-registry 1.4.8, siphasher 1.0.4, smallvec 1.16.4, socket2 0.6.5, stable_deref_trait 1.2.1, strength_reduce 0.2.4, string_cache 0.9.0, string_cache_codegen 0.6.1, syn 1.0.109, syn 2.0.119, syn 3.0.9, system-deps 6.2.2, tao-macros 0.1.4, tendril 0.5.1, thiserror 1.0.69, thiserror 2.0.21, thiserror-impl 1.0.69, thiserror-impl 2.0.21, time 0.3.55, time-core 0.1.9, time-macros 0.2.32, toml 0.8.2, toml_datetime 0.6.3, toml_datetime 1.1.2+spec-1.1.0, toml_edit 0.19.15, toml_edit 0.20.2, toml_edit 0.25.17+spec-1.1.0, toml_parser 1.1.5+spec-1.1.0, transpose 0.2.3, tray-icon 0.26.1, tungstenite 0.29.0, typenum 1.20.1, unicase 2.10.0, unicode-segmentation 1.13.3, ureq 3.5.0, ureq-proto 0.6.5, url 2.5.8, utf8-zero 0.8.1, wasm-bindgen 0.2.129, wasm-bindgen-macro 0.2.129, wasm-bindgen-macro-support 0.2.129, wasm-bindgen-shared 0.2.129, web_atoms 0.2.6, windows 0.62.2, windows-collections 0.3.2, windows-core 0.62.2, windows-future 0.3.2, windows-implement 0.60.2, windows-interface 0.59.3, windows-link 0.2.1, windows-numerics 0.3.1, windows-result 0.4.1, windows-strings 0.5.1, windows-sys 0.45.0, windows-sys 0.52.0, windows-sys 0.61.2, windows-targets 0.42.2, windows-targets 0.52.6, windows-threading 0.2.1, windows-version 0.1.7, windows_aarch64_gnullvm 0.42.2, windows_aarch64_gnullvm 0.52.6, windows_aarch64_msvc 0.42.2, windows_aarch64_msvc 0.52.6, windows_i686_gnu 0.42.2, windows_i686_gnu 0.52.6, windows_i686_gnullvm 0.52.6, windows_i686_msvc 0.42.2, windows_i686_msvc 0.52.6, windows_x86_64_gnu 0.42.2, windows_x86_64_gnu 0.52.6, windows_x86_64_gnullvm 0.42.2, windows_x86_64_gnullvm 0.52.6, windows_x86_64_msvc 0.42.2, windows_x86_64_msvc 0.52.6
- **MIT** (76): atk 0.18.2, atk-sys 0.18.2, axum 0.8.9, axum-core 0.5.6, block2 0.6.2, bytes 1.12.1, cairo-rs 0.18.5, cairo-sys-rs 0.18.2, combine 4.6.9, data-encoding 2.11.1, derive_more 2.1.1, derive_more-impl 2.1.1, dlopen2 0.8.2, dlopen2_derive 0.4.3, dom_query 0.28.0, gdk 0.18.2, gdk-pixbuf 0.18.5, gdk-pixbuf-sys 0.18.0, gdk-sys 0.18.2, gdkwayland-sys 0.18.2, generic-array 0.14.7, gio 0.18.4, gio-sys 0.18.1, glib 0.18.5, glib-macros 0.18.5, glib-sys 0.18.1, gobject-sys 0.18.0, gtk 0.18.2, gtk-sys 0.18.2, gtk3-macros 0.18.2, http-body 1.1.0, http-body-util 0.1.5, http-range-header 0.4.2, hyper 1.12.0, hyper-util 0.1.21, libredox 0.1.25, memoffset 0.9.1, mime_guess 2.0.5, mio 1.2.4, new_debug_unreachable 1.0.6, objc2 0.6.5, objc2-encode 4.1.0, objc2-foundation 0.3.2, pango 0.18.3, pango-sys 0.18.0, phf 0.13.1, phf_codegen 0.13.1, phf_generator 0.13.1, phf_macros 0.13.1, phf_shared 0.13.1, precomputed-hash 0.1.1, redox_syscall 0.5.18, redox_users 0.5.3, rust-embed 8.13.0, rust-embed-impl 8.13.0, rust-embed-utils 8.13.0, simd-adler32 0.3.10, slab 0.4.12, synstructure 0.14.0, tokio 1.54.1, tokio-macros 2.7.2, tokio-tungstenite 0.29.0, tokio-util 0.7.20, tower 0.5.3, tower-http 0.6.11, tower-layer 0.3.3, tower-service 0.3.3, tracing 0.1.44, tracing-core 0.1.36, version-compare 0.2.1, webview2-com 0.39.1, webview2-com-macros 0.8.1, webview2-com-sys 0.39.1, winnow 0.5.40, winnow 1.0.4, zmij 1.0.23
- **Unicode-3.0** (18): icu_collections 2.4.0, icu_locale_core 2.4.0, icu_normalizer 2.4.0, icu_normalizer_data 2.4.0, icu_properties 2.4.0, icu_properties_data 2.4.0, icu_provider 2.4.0, litemap 0.8.3, potential_utf 0.1.6, tinystr 0.8.5, writeable 0.6.4, yoke 0.8.4, yoke-derive 0.8.4, zerofrom 0.1.8, zerofrom-derive 0.1.8, zerotrie 0.2.5, zerovec 0.11.9, zerovec-derive 0.11.7
- **Apache-2.0 OR MIT** (15): atomic-waker 1.1.2, autocfg 1.5.1, bit-set 0.8.0, bit-vec 0.8.0, const-oid 0.10.2, equivalent 1.0.3, fastrand 2.5.0, idna_adapter 1.2.2, indexmap 2.14.2, muda 0.21.2, pin-project-lite 0.2.17, rustc-hash 2.1.3, utf8_iter 1.0.4, wry 0.57.1, zeroize 1.9.1
- **Zlib OR Apache-2.0 OR MIT** (15): dispatch2 0.3.1, objc2-app-kit 0.3.2, objc2-cloud-kit 0.3.2, objc2-core-data 0.3.2, objc2-core-foundation 0.3.2, objc2-core-graphics 0.3.2, objc2-core-image 0.3.2, objc2-core-location 0.3.2, objc2-core-text 0.3.2, objc2-exception-helper 0.1.1, objc2-io-surface 0.3.2, objc2-quartz-core 0.3.2, objc2-ui-kit 0.3.2, objc2-user-notifications 0.3.2, objc2-web-kit 0.3.2
- **MIT/Apache-2.0** (9): foreign-types 0.5.0, foreign-types-macros 0.2.4, foreign-types-shared 0.3.1, jni 0.21.1, serde_urlencoded 0.7.1, version_check 0.9.5, winapi 0.3.9, winapi-i686-pc-windows-gnu 0.4.0, winapi-x86_64-pc-windows-gnu 0.4.0
- **MPL-2.0** (5): cssparser 0.37.0, cssparser-macros 0.7.1, dtoa-short 0.3.5, option-ext 0.2.0, selectors 0.38.0
- **Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT** (3): wasi 0.11.1+wasi-snapshot-preview1, wasip2 1.0.4+wasi-0.2.12, wit-bindgen 0.57.1
- **ISC** (3): libloading 0.8.9, rustls-webpki 0.103.16, untrusted 0.9.0
- **Apache-2.0** (2): sync_wrapper 1.0.2, tao 0.37.2
- **BSD-2-Clause OR Apache-2.0 OR MIT** (2): zerocopy 0.8.63, zerocopy-derive 0.8.63
- **BSD-3-Clause OR MIT OR Apache-2.0** (2): num_enum 0.7.6, num_enum_derive 0.7.6
- **MIT OR Zlib OR Apache-2.0** (2): miniz_oxide 0.8.9, miniz_oxide 0.9.1
- **Unlicense OR MIT** (2): memchr 2.8.3, winapi-util 0.1.11
- **Unlicense/MIT** (2): same-file 1.0.6, walkdir 2.5.0
- **Zlib** (2): foldhash 0.2.0, zlib-rs 0.6.8
- **(MIT OR Apache-2.0) AND Unicode-3.0** (1): unicode-ident 1.0.27
- **0BSD OR MIT OR Apache-2.0** (1): adler2 2.0.1
- **Apache-2.0 AND ISC** (1): ring 0.17.14
- **Apache-2.0 AND MIT** (1): dpi 0.1.2
- **Apache-2.0 OR BSL-1.0** (1): ryu 1.0.23
- **Apache-2.0 OR ISC OR MIT** (1): rustls 0.23.46
- **Apache-2.0 WITH LLVM-exception** (1): target-lexicon 0.12.16
- **Apache-2.0/MIT** (1): cesu8 1.1.0
- **BSD-3-Clause** (1): subtle 2.6.1
- **CC0-1.0 OR MIT-0 OR Apache-2.0** (1): dunce 1.0.5
- **CDLA-Permissive-2.0** (1): webpki-roots 1.0.11
- **MIT AND BSD-3-Clause** (1): matchit 0.8.4
- **MIT OR Apache-2.0 OR LGPL-2.1-or-later** (1): r-efi 5.3.0
- **MIT OR Apache-2.0 OR Zlib** (1): raw-window-handle 0.6.2
