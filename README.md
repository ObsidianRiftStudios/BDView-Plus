# TuxInfo

TuxInfo is a Blu-ray / UHD disc analyzer written in Rust. The scanner/CLI are std only; the optional GUI uses egui. It
reads a Blu-ray `BDMV` folder and prints the same kind of report (playlists, video/audio/subtitle
streams, per-clip and per-chapter bitrates, stream diagnostics, quick summary).

    cargo build --release
    tuxinfo disc.iso                 # UDF image (ISO)
    tuxinfo /dev/sr0                 # disc in a drive (raw device; Windows: \\.\E:)
    tuxinfo /path/to/disc            # report for all playlists >= 20 s without loops
    tuxinfo /path/to/disc -l         # list playlists
    tuxinfo /path/to/disc -p 00800 -o report.txt
    tuxinfo --help

## GUI
`tuxinfo` with no arguments (or `tuxinfo --gui [path]`) opens a native window (egui/eframe) laid out like the
classic three-list layout: source box with Browse (folder), Browse ISO, playlist / stream-file / stream lists, log,
progress with time remaining/elapsed, Scan Bitrates (Cancel while running), View Report (copy/save) and
Settings. A folder, ISO or device can also be dropped onto the window. Native file dialogs use rfd
(xdg-desktop-portal on Linux).

- The source box is pre-filled with the optical drive; press **Scan** (it reads **Rescan** after the first
  scan), or Browse for a folder/ISO.
- Drag the thin bars between the four boxes to resize them, and drag the column lines to resize columns.
  Boxes and columns grow in proportion when the window is enlarged; scroll bars appear when there is more data.
- **Scan Bitrates** reads the whole video/audio files of the selected playlists to measure the real bit
  rates (fills Measured Bytes / Bit Rate and the report). It can take many minutes on a full disc.
- Opening a disc shows its structure (playlists, clips, streams from the clip info files) straight away and
  reads no stream data. **Scan Bitrates** then identifies the streams and measures the bit rates; tick only
  the playlists you care about to read less. **Cancel Scan** stops it. With a source box left empty, Scan uses
  the optical drive (the one holding a Blu-ray if there are several).
- Settings has the text size (default 16 pt, sample box, Apply). **View Report** opens a separate window
  that can be moved outside the main one; it has its own text size (- / +, or Ctrl+wheel).

The GUI is the default Cargo feature `gui`; `cargo build --release --no-default-features` builds the
CLI-only program with no dependencies at all.

## Preview

Select a playlist (or a stream file in the middle list) and press **Play List** / **Play Clip**, double-click a row, or use **Play File...** to open any video file. The video plays in a preview window with seek, volume, and audio/subtitle choice. Playlists go through mpv's `bd://` protocol, so encrypted discs decrypt the same way as the scan (libbluray + MakeMKV's libmmbd).

The preview needs libmpv at run time (Debian/Ubuntu: `sudo apt install libmpv2`); it is loaded on first use, adds about 60 KB to the executable, and the rest of TuxInfo works without it. Build without it using `--no-default-features --features gui,bluray`.

## Install
`packaging/install.sh` builds the release binary and installs it, with an application-menu entry, under
`~/.local` (`--prefix DIR` to change, `uninstall` to remove). Or just `cargo build --release`.

## Notes
- Input is a folder (or the BDMV folder itself), a UDF image, or a raw disc device (read-only UDF 1.02–2.50,
  including metadata partitions). A mounted disc works as a folder. Encrypted discs are read through libbluray
  (see below); with `--no-bluray` they must be decrypted first. Virtual-partition (VAT) UDF is unsupported.
- No SSIF / Blu-ray 3D / MVC and no per-angle stream accounting (angle clips are listed but excluded from totals).
- HEVC VUI is parsed only up to timing info; the quick identification pass stops after 48 MiB per file.
- Video length uses PTS when a stream has no DTS.
- Report lines end in `\n`; the "TuxInfo:" line shows the program version.
- UDF reading is tested on UDF 2.60 ISOs (physical partition); the metadata-partition path and raw devices are untested.
- Tested on synthetic discs and ffmpeg-made MPEG-2, H.264, HEVC (HDR10), AC-3, DTS, TrueHD and LPCM
  streams; not yet on retail discs, and the E-AC-3/Atmos, DTS-HD, VC-1 and PGS paths are untested.

## License and credits
LGPL-2.1-or-later (see `LICENSE`). TuxInfo is based on BDInfo by Cinema Squid, continued by the UniqProject developers (https://github.com/UniqProject/BDInfo), which is LGPL-2.1 licensed. The same acknowledgement is in the program's About window.

## Encrypted discs (libbluray / MakeMKV)
Playlists and clip info are not encrypted, but the stream files are, and the scanner has to read them. For every
Blu-ray and 4K disc (folder, ISO or drive) the program loads an installed **libbluray** at run time (nothing is
linked) and reads the stream files through it, the same way VLC does. libbluray finds `libaacs`, or MakeMKV's
`libmmbd` set up as `libaacs` (see the MakeMKV forum thread "Direct Blu-ray playback with VLC"); `libmmbd` in turn
needs `makemkvcon` reachable. Flatpak MakeMKV is not visible from outside its sandbox, so install MakeMKV from
source on Linux. If libbluray is not installed the files are read directly, which works for decrypted data only.

- Debian: `apt install libbluray2`; Windows: the `libbluray-2.dll` that ships with VLC is found when it is on the PATH
  or next to the program. Set `TUXINFO_LIBBLURAY` to a full path to override the search.
- `--no-bluray` skips libbluray and reads the files directly.
- AACS only; BD+ discs may still produce corrupt data (a reported libmmbd problem).
- Build without it (and without any FFI): `cargo build --release --no-default-features` (add `--features gui` for the window).

If MakeMKV's `libmmbd.so.0` is installed (in `/usr/lib`, `/usr/local/lib`, `/opt/makemkv/lib`, or on the library
path), TuxInfo tells libbluray to use it for AACS/BD+ instead of libaacs (libbluray would otherwise pick an
installed libaacs that has no key file). Set `TUXINFO_MMBD=<dir containing libmmbd.so.0>` if it is elsewhere, or
`TUXINFO_NO_MMBD=1` to keep libbluray's default. MakeMKV's `makemkvcon` must be on PATH and MakeMKV registered.
