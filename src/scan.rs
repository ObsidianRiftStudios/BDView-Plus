//! Scan orchestration shared by the CLI and the GUI.

use crate::disc::Disc;
use crate::ts;

pub struct Progress<'a> {
    pub label: &'static str,
    pub file: &'a str,
    pub done: u64,
    pub total: u64,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    /// Identify streams only (quick pass) and build the playlist stream tables.
    Identify,
    /// Bitrate scan of already-identified playlists.
    Bitrates,
    Both,
}

/// Run the requested pass(es) over the files of the playlists in `sel`.
/// `on` is called regularly; returning false cancels. Returns warnings.
pub fn scan(disc: &mut Disc, sel: &[usize], mode: Mode, on: &mut dyn FnMut(&Progress) -> bool) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut order: Vec<String> = Vec::new();
    for &i in sel {
        for c in &disc.playlists[i].clips {
            if c.has_file && !order.contains(&c.name) {
                order.push(c.name.clone());
            }
        }
    }
    if mode != Mode::Bitrates {
        // file names run in the order the files sit on the disc, so the head sweeps once instead of jumping about
        let mut by_name = order.clone();
        by_name.sort();
        let ok = pass(disc, &by_name, false, sel, "Identifying", on, &mut warnings);
        disc.finish_sel(sel);
        if !ok {
            return warnings;
        }
    }
    if mode != Mode::Identify {
        for &i in sel {
            disc.playlists[i].clear_bitrates(&mut disc.files);
        }
        if pass(disc, &order, true, sel, "Scanning", on, &mut warnings) {
            disc.update_captions(sel);
        }
    }
    warnings
}

fn pass(
    disc: &mut Disc,
    names: &[String],
    full: bool,
    sel: &[usize],
    label: &'static str,
    on: &mut dyn FnMut(&Progress) -> bool,
    warnings: &mut Vec<String>,
) -> bool {
    // The quick pass reads only the start of each file, so its progress counts files (in 1000ths)
    // rather than bytes; the full pass counts bytes.
    let total: u64 = if full { names.iter().map(|n| disc.files[n].size).sum() } else { names.len() as u64 * 1000 };
    let mut done = 0u64;
    for name in names {
        let mut f = disc.files.remove(name).unwrap();
        let mine: Vec<usize> = if full {
            sel.iter().copied().filter(|&i| disc.playlists[i].clips.iter().any(|c| &c.name == name)).collect()
        } else {
            Vec::new()
        };
        let (base, size) = (done, f.size);
        let mut cancelled = false;
        let r = ts::scan_file(&mut f, &mut disc.playlists, &mine, full, &mut |b| {
            let d = if full { base + b } else { base + (b * 1000 / ts::QUICK_CAP).min(999) };
            let keep = on(&Progress { label, file: name, done: d, total });
            cancelled |= !keep;
            keep
        });
        done += if full { size } else { 1000 };
        disc.files.insert(name.clone(), f);
        if cancelled {
            warnings.push("Scan was cancelled.".into());
            return false;
        }
        if let Err(e) = r {
            warnings.push(format!("{name}\t{e}"));
        }
    }
    on(&Progress { label, file: "", done: total, total });
    true
}
