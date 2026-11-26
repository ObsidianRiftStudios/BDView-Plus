//! bdview-plus — Blu-ray disc structure and bitrate analyzer, LGPL-2.1.

mod app_info;
mod bits;
#[cfg(feature = "bluray")]
mod bluray;
#[cfg(feature = "gui")]
mod chart;
mod codecs;
mod disc;
#[cfg(feature = "gui")]
mod gui;
mod hevc;
mod lang;
#[cfg(all(feature = "preview", unix))]
mod mpv;
#[cfg(all(feature = "preview", unix))]
mod preview;
mod report;
mod scan;
#[cfg(feature = "gui")]
mod scancache;
mod stream;
mod ts;
mod vfs;

use disc::Disc;
use std::path::PathBuf;
use std::time::Instant;

const USAGE: &str = "usage: bdview-plus <BDMV folder | ISO image | disc device (e.g. /dev/sr0)> [options]
  -l, --list         list playlists and exit
  -p, --playlists A,B  only report these playlists (e.g. 00800.MPLS,00801)
  -o, --output PATH  write the report to a file (or into a directory)
      --all          include short (<20 s) and looping playlists
      --sort         order playlists by length (longest first)
      --quick        identify streams only; skip the bitrate scan
      --no-diag      omit the STREAM DIAGNOSTICS table
      --extended     wait for HDR metadata before finishing HEVC streams
      --no-bluray    read stream files directly instead of through libbluray (decrypted data only)
  -q, --quiet        no progress output
      --gui          open the graphical window (default with no arguments)";

fn fail(msg: &str) -> ! {
    eprintln!("bdview-plus: {msg}");
    std::process::exit(1);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut path, mut out, mut names) = (None, None, Vec::new());
    let mut gui = false;
    let (mut list, mut all, mut sort, mut quick, mut diag, mut quiet) = (false, false, false, false, true, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => return println!("{USAGE}"),
            "-l" | "--list" => list = true,
            "--all" => all = true,
            "--sort" => sort = true,
            "--quick" => quick = true,
            "--no-diag" => diag = false,
            #[cfg(feature = "bluray")]
            "--no-bluray" => bluray::DISABLE.store(true, std::sync::atomic::Ordering::Relaxed),
            "--extended" => hevc::EXTENDED.store(true, std::sync::atomic::Ordering::Relaxed),
            "-q" | "--quiet" => quiet = true,
            "--gui" => gui = true,
            "-o" | "--output" => out = Some(PathBuf::from(args.next().unwrap_or_else(|| fail("-o needs a path")))),
            "-p" | "--playlists" => {
                let v = args.next().unwrap_or_else(|| fail("-p needs a list"));
                names.extend(v.split(',').map(|s| {
                    let s = s.trim().to_uppercase();
                    if s.ends_with(".MPLS") {
                        s
                    } else {
                        format!("{s}.MPLS")
                    }
                }));
            }
            s if s.starts_with('-') => fail(&format!("unknown option {s}\n{USAGE}")),
            _ => path = Some(PathBuf::from(a)),
        }
    }
    if gui || (path.is_none() && std::env::args().len() == 1) {
        #[cfg(feature = "gui")]
        return gui::run(path.map(|p| p.display().to_string()));
        #[cfg(not(feature = "gui"))]
        fail("this build has no GUI (rebuild with --features gui)");
    }
    let Some(path) = path else {
        fail(&format!("no path given\n{USAGE}"))
    };
    let mut disc = Disc::open(&path).unwrap_or_else(|e| fail(&e));
    let mut warnings = std::mem::take(&mut disc.errors);
    if !quiet {
        for w in &warnings {
            eprintln!("warning: {w}");
        }
    }

    // choose playlists
    let mut sel: Vec<usize> = (0..disc.playlists.len())
        .filter(|&i| {
            let p = &disc.playlists[i];
            if names.is_empty() {
                all || (p.total_length() >= 20.0 && !p.detect_loops())
            } else {
                names.contains(&p.name)
            }
        })
        .collect();
    if let Some(n) = names.iter().find(|n| !disc.playlists.iter().any(|p| &p.name == *n)) {
        fail(&format!("playlist {n} not found"));
    }
    if sort {
        sel.sort_by(|&a, &b| {
            let (x, y) = (&disc.playlists[a], &disc.playlists[b]);
            y.total_length().total_cmp(&x.total_length()).then_with(|| x.name.cmp(&y.name))
        });
    }
    if list {
        println!("{:<14}{:<14}{:>18}{:>7}  Listed", "Name", "Length", "Size (bytes)", "Clips");
        for i in 0..disc.playlists.len() {
            let p = &disc.playlists[i];
            println!(
                "{:<14}{:<14}{:>18}{:>7}  {}",
                p.name,
                fmt_len(p.total_length()),
                report::n0(p.file_size() as i64),
                p.clips.iter().filter(|c| c.angle == 0).count(),
                if sel.contains(&i) { "yes" } else { "no" }
            );
        }
        return;
    }
    if sel.is_empty() {
        fail("no playlists to report (try --all or -p)");
    }

    let mode = if quick { scan::Mode::Identify } else { scan::Mode::Both };
    let mut last = Instant::now();
    warnings.extend(scan::scan(&mut disc, &sel, mode, &mut |p| {
        if !quiet {
            if p.total > 0 && p.done == p.total {
                eprintln!("\r{} done.{:<60}", p.label, "");
            } else if last.elapsed().as_millis() > 500 {
                last = Instant::now();
                let pct = (100 * p.done).checked_div(p.total).unwrap_or(100);
                eprint!("\r{} {}: {pct}%  ({} MB/{} MB)  ", p.label, p.file, p.done >> 20, p.total >> 20);
            }
        }
        true
    }));

    let text = report::generate(
        &disc,
        &sel,
        &report::Options {
            diag,
            summary: true,
            warnings,
        },
    );
    match out {
        None => print!("{text}"),
        Some(mut p) => {
            if p.is_dir() || p.as_os_str().to_string_lossy().ends_with('/') {
                let safe: String = disc
                    .label
                    .chars()
                    .map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
                    .collect();
                p.push(format!("BDViewPlus.{safe}.txt"));
            }
            if let Some(d) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                let _ = std::fs::create_dir_all(d);
            }
            std::fs::write(&p, text).unwrap_or_else(|e| fail(&format!("{}: {e}", p.display())));
            if !quiet {
                eprintln!("report written to {}", p.display());
            }
        }
    }
}

fn fmt_len(s: f64) -> String {
    let t = s as u64;
    format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
}
