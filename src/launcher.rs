//! Puts Inkwell in the desktop's app menu (a freedesktop `.desktop` entry plus an icon in
//! the user's own folders), so it can be started like any other app and comics can be
//! opened with it from the file manager.

use std::{fs, io, path::PathBuf};

use image::{Rgba, RgbaImage};

fn applications_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("applications"))
}

fn icon_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("icons/hicolor/256x256/apps/inkwell.png"))
}

/// A rose rounded square with a white nib, drawn here so nothing has to be shipped.
fn draw_icon() -> RgbaImage {
    let n = 256u32;
    let mut img = RgbaImage::new(n, n);
    let rose = [0xf4u8, 0x3f, 0x5e];
    let r = 56.0f32;
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = fx.clamp(r, n as f32 - r);
            let cy = fy.clamp(r, n as f32 - r);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let a = (r - d + 0.5).clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            // Nib: a diamond with a slit, centred.
            let (dx, dy) = (fx - 128.0, fy - 120.0);
            let nib = dx.abs() / 46.0 + (dy - 10.0).abs() / 86.0 <= 1.0 && dy > -60.0;
            let slit = dx.abs() < 4.0 && dy > -10.0 && dy < 70.0;
            let hole = (dx * dx + (dy + 10.0).powi(2)).sqrt() < 10.0;
            let white = nib && !slit && !hole;
            let c = if white { [255, 255, 255] } else { rose };
            img.put_pixel(x, y, Rgba([c[0], c[1], c[2], (a * 255.0) as u8]));
        }
    }
    img
}

/// The desktop entry's text for the program at `exe`.
fn entry(exe: &str, icon: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Inkwell\n\
         GenericName=Manga translator\n\
         Comment=Drop comics, translate the text, export a new CBZ\n\
         Exec=\"{exe}\" %F\n\
         Icon={icon}\n\
         Terminal=false\n\
         Categories=Graphics;Viewer;\n\
         MimeType=application/vnd.comicbook+zip;application/x-cbz;application/zip;\n\
         StartupWMClass=inkwell\n"
    )
}

/// Writes (or refreshes) the app-menu entry for the running program. Returns the entry's
/// path, or `None` when it was already current.
/// Where the app-menu entry's copy of the program lives. Running a copy, not the program
/// in the build folder, means a rebuild can't pull the program out from under an open
/// window (which ends it at once, with no chance to save a crash note).
fn installed_program() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("inkwell/bin/inkwell"))
}

/// Copies the running program to `installed_program()` when it differs (size or time),
/// through a temporary file renamed into place, so a copy that is running keeps going.
fn install_copy(exe: &std::path::Path) -> io::Result<PathBuf> {
    let Some(dest) = installed_program() else { return Ok(exe.to_owned()) };
    if exe == dest {
        return Ok(dest);
    }
    let same = match (fs::metadata(exe), fs::metadata(&dest)) {
        (Ok(a), Ok(b)) => a.len() == b.len() && a.modified().ok() <= b.modified().ok(),
        _ => false,
    };
    if !same {
        if let Some(dir) = dest.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = crate::project::with_suffix(&dest, ".new");
        fs::copy(exe, &tmp)?;
        fs::rename(&tmp, &dest)?;
    }
    Ok(dest)
}

pub fn install(force: bool) -> io::Result<Option<PathBuf>> {
    let exe = std::env::current_exe()?;
    let exe = install_copy(&exe)?;
    let exe = exe.to_string_lossy().replace('"', "\\\"");
    let (Some(apps), Some(icon)) = (applications_dir(), icon_path()) else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no home folder"));
    };
    let path = apps.join("inkwell.desktop");
    let text = entry(&exe, &icon.to_string_lossy());
    if !force && fs::read_to_string(&path).map(|t| t == text).unwrap_or(false) && icon.exists() {
        return Ok(None);
    }
    if let Some(dir) = icon.parent() {
        fs::create_dir_all(dir)?;
    }
    draw_icon()
        .save(&icon)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    fs::create_dir_all(&apps)?;
    crate::project::atomic_write(&path, text.as_bytes())?;
    let _ = fs::remove_file(crate::project::with_suffix(&path, ".bak"));
    Ok(Some(path))
}
