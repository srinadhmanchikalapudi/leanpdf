//! Printing: the standard Windows print dialog, then each page is drawn by PDFium at the
//! printer's resolution and sent to the printer with GDI. Runs on its own thread so the
//! window stays usable.

use crate::*;
use leanpdf_core::pdf::Cmd;
use std::sync::mpsc::Sender;

#[cfg(windows)]
mod win {
    use super::*;
    use windows_sys::Win32::Graphics::Gdi::{
        CreateDCW, DeleteDC, GetDeviceCaps, SetStretchBltMode, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HALFTONE, HDC, HORZRES, SRCCOPY, VERTRES,
    };
    use windows_sys::Win32::Storage::Xps::{EndDoc, EndPage, StartDocW, StartPage, DOCINFOW};
    use windows_sys::Win32::UI::Controls::Dialogs::{PrintDlgW, PD_ALLPAGES, PD_NOSELECTION, PD_PAGENUMS, PD_RETURNDC, PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGW};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;

    pub struct SendDc(pub HDC);
    // A device context may be used from the thread that prints; nothing else touches it meanwhile.
    unsafe impl Send for SendDc {}

    pub struct Chosen {
        pub dc: SendDc,
        /// Zero-based, end exclusive.
        pub from: usize,
        pub to: usize,
        pub output: Option<String>,
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Shows the print dialog. `None` when the person cancels.
    /// For automated tests, `LEANPDF_PRINT_TEST_FILE` skips the dialog and prints all pages with
    /// "Microsoft Print to PDF" into that file.
    pub fn choose(total: usize) -> Option<Chosen> {
        unsafe {
            if let Ok(file) = std::env::var("LEANPDF_PRINT_TEST_FILE") {
                let name = wide("Microsoft Print to PDF");
                let dc = CreateDCW(wide("WINSPOOL").as_ptr(), name.as_ptr(), std::ptr::null(), std::ptr::null());
                if dc.is_null() {
                    return None;
                }
                return Some(Chosen { dc: SendDc(dc), from: 0, to: total, output: Some(file) });
            }
            let mut pd: PRINTDLGW = std::mem::zeroed();
            pd.lStructSize = std::mem::size_of::<PRINTDLGW>() as u32;
            pd.hwndOwner = GetActiveWindow();
            pd.Flags = PD_RETURNDC | PD_NOSELECTION | PD_USEDEVMODECOPIESANDCOLLATE | PD_ALLPAGES;
            let max = total.min(65535) as u16;
            pd.nMinPage = 1;
            pd.nMaxPage = max;
            pd.nFromPage = 1;
            pd.nToPage = max;
            pd.nCopies = 1;
            if PrintDlgW(&mut pd) == 0 || pd.hDC.is_null() {
                return None;
            }
            let (from, to) = if pd.Flags & PD_PAGENUMS != 0 {
                let f = (pd.nFromPage as usize).clamp(1, total);
                let t = (pd.nToPage as usize).clamp(f, total);
                (f - 1, t)
            } else {
                (0, total)
            };
            Some(Chosen { dc: SendDc(pd.hDC), from, to, output: None })
        }
    }

    /// Prints pages `from..to`. `sizes` are the pages' sizes as they are shown (after `rot`).
    #[allow(clippy::too_many_arguments)]
    pub fn run(chosen: Chosen, viewer: Sender<Cmd>, doc: u32, name: String, rot: u8, sizes: Vec<[f32; 2]>, say: &dyn Fn(String)) -> Result<usize, String> {
        unsafe {
            let hdc = chosen.dc.0;
            let docname = wide(&name);
            let out = chosen.output.as_deref().map(wide);
            let info = DOCINFOW {
                cbSize: std::mem::size_of::<DOCINFOW>() as i32,
                lpszDocName: docname.as_ptr(),
                lpszOutput: out.as_ref().map_or(std::ptr::null(), |o| o.as_ptr()),
                lpszDatatype: std::ptr::null(),
                fwType: 0,
            };
            if StartDocW(hdc, &info) <= 0 {
                DeleteDC(hdc);
                return Err("The printer did not accept the document.".into());
            }
            let (pw, ph) = (GetDeviceCaps(hdc, HORZRES as i32), GetDeviceCaps(hdc, VERTRES as i32));
            if pw <= 0 || ph <= 0 {
                EndDoc(hdc);
                DeleteDC(hdc);
                return Err("Could not read the paper size from the printer.".into());
            }
            SetStretchBltMode(hdc, HALFTONE);
            let count = chosen.to - chosen.from;
            let mut printed = 0usize;
            let mut failure = None;
            for (n, i) in (chosen.from..chosen.to).enumerate() {
                say(format!("Printing page {} of {}…", n + 1, count));
                let shown = sizes.get(i).copied().unwrap_or([612.0, 792.0]);
                // turn a landscape page to fit portrait paper (and the reverse) so it prints as large as possible
                let turn = ((pw > ph) != (shown[0] > shown[1])) as u8;
                let (dw, dh) = if turn == 1 { (shown[1], shown[0]) } else { (shown[0], shown[1]) };
                let scale = (pw as f32 / dw).min(ph as f32 / dh);
                let width = ((dw * scale).round() as u32).clamp(100, 4000);
                let (rtx, rrx) = std::sync::mpsc::channel();
                if viewer.send(Cmd::RenderRgba { doc, page: i as u32, width, rot: (rot + turn) % 4, reply: Box::new(move |r| drop(rtx.send(r))) }).is_err() {
                    failure = Some("LeanPDF stopped while printing.".to_string());
                    break;
                }
                let (w, h, mut px) = match rrx.recv_timeout(std::time::Duration::from_secs(60)) {
                    Ok(Ok(img)) => img,
                    Ok(Err(e)) => {
                        failure = Some(format!("Page {} could not be drawn: {e}", i + 1));
                        break;
                    }
                    Err(_) => {
                        failure = Some(format!("Page {} took too long to draw.", i + 1));
                        break;
                    }
                };
                for p in px.chunks_exact_mut(4) {
                    p.swap(0, 2); // RGBA -> BGRA
                }
                let mut bmi: BITMAPINFO = std::mem::zeroed();
                bmi.bmiHeader = BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32), // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                };
                StartPage(hdc);
                let x = ((pw - w as i32) / 2).max(0);
                let y = ((ph - h as i32) / 2).max(0);
                StretchDIBits(hdc, x, y, w as i32, h as i32, 0, 0, w as i32, h as i32, px.as_ptr().cast(), &bmi, DIB_RGB_COLORS, SRCCOPY);
                EndPage(hdc);
                printed += 1;
            }
            EndDoc(hdc);
            DeleteDC(hdc);
            match failure {
                Some(e) => Err(e),
                None => Ok(printed),
            }
        }
    }
}

impl App {
    /// File > Print (Ctrl+P).
    pub fn print_doc(&mut self) {
        if self.print_rx.is_some() {
            self.toast("Already printing.");
            return;
        }
        let Some(d) = &self.doc else { return };
        #[cfg(windows)]
        {
            let total = d.pages.len();
            let (id, name, rot) = (d.id, d.name.clone(), d.rot);
            let sizes: Vec<[f32; 2]> = (0..total).map(|i| d.disp(i)).collect();
            let Some(chosen) = win::choose(total) else { return };
            let (tx, rx) = channel::<String>();
            self.print_rx = Some(rx);
            let viewer = self.viewer.clone();
            let ctx = self.ctx.clone();
            std::thread::spawn(move || {
                let say = |m: String| {
                    let _ = tx.send(m);
                    ctx.request_repaint();
                };
                let end = match win::run(chosen, viewer, id, name, rot, sizes, &say) {
                    Ok(n) => format!("DONE:Sent {n} page{} to the printer.", if n == 1 { "" } else { "s" }),
                    Err(e) => format!("DONE:{e}"),
                };
                let _ = tx.send(end);
                ctx.request_repaint();
            });
            self.toast("Printing…");
        }
        #[cfg(not(windows))]
        {
            let _ = d;
            self.toast("Printing is only available on Windows.");
        }
    }

    /// Shows progress messages from a print job. Called every frame.
    pub fn poll_print(&mut self) {
        let Some(rx) = &self.print_rx else { return };
        let mut finished = false;
        let mut last = None;
        while let Ok(m) = rx.try_recv() {
            match m.strip_prefix("DONE:") {
                Some(rest) => {
                    last = Some(rest.to_string());
                    finished = true;
                }
                None => last = Some(m),
            }
        }
        if finished {
            self.print_rx = None;
        }
        if let Some(m) = last {
            self.toast(m);
        }
    }
}
