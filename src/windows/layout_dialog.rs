use super::platform::*;
use multimachine_rust::{layout::Layout, model::*};
use std::cell::RefCell;
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::*,
};
struct Dialog {
    window: HWND,
    preview: HWND,
    side: HWND,
    offset: HWND,
    local: Desktop,
    remote: Desktop,
    placement: Placement,
    done: bool,
    result: Option<Placement>,
    drag: Option<(i32, i32)>,
    scale: f64,
}
thread_local! {static DIALOG:RefCell<Option<Dialog>>=const{RefCell::new(None)};}
pub fn show(
    owner: HWND,
    local: Desktop,
    remote: Desktop,
    placement: Placement,
) -> Option<Placement> {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: module(),
            lpszClassName: w!("MultiMachineRust.Layout"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as *mut _),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let mut preview_class = wc;
        preview_class.lpszClassName = w!("MultiMachineRust.Preview");
        preview_class.lpfnWndProc = Some(preview_proc);
        RegisterClassW(&preview_class);
        let window = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            w!("MultiMachineRust.Layout"),
            w!("Disposition des écrans"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            760,
            500,
            Some(owner),
            None,
            Some(module()),
            None,
        )
        .ok()?;
        control(
            window,
            "STATIC",
            "Placez l’autre PC à gauche ou à droite, puis ajustez sa hauteur.",
            0,
            20,
            18,
            710,
            24,
            0,
        );
        control(
            window,
            "STATIC",
            "Dans l’aperçu, glissez verticalement pour ajuster le passage.",
            0,
            20,
            46,
            710,
            24,
            0,
        );
        let preview = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("MultiMachineRust.Preview"),
            w!(""),
            WS_CHILD | WS_VISIBLE,
            20,
            82,
            700,
            245,
            Some(window),
            None,
            Some(module()),
            None,
        )
        .ok()?;
        control(window, "STATIC", "Autre PC :", 0, 22, 348, 100, 24, 0);
        let side = control(
            window,
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32,
            125,
            343,
            170,
            100,
            1,
        );
        for text in ["À droite", "À gauche"] {
            SendMessageW(
                side,
                CB_ADDSTRING,
                None,
                Some(LPARAM(wide(text).as_ptr() as isize)),
            );
        }
        SendMessageW(
            side,
            CB_SETCURSEL,
            Some(WPARAM(if placement.right { 0 } else { 1 })),
            None,
        );
        control(
            window,
            "STATIC",
            "Décalage vertical (pixels) :",
            0,
            315,
            348,
            230,
            24,
            0,
        );
        let offset = control(
            window,
            "EDIT",
            &placement.offset.to_string(),
            WS_BORDER.0 | ES_AUTOHSCROLL as u32,
            560,
            343,
            115,
            28,
            2,
        );
        control(
            window,
            "BUTTON",
            "Annuler",
            BS_PUSHBUTTON as u32,
            455,
            406,
            115,
            32,
            3,
        );
        control(
            window,
            "BUTTON",
            "Appliquer",
            BS_DEFPUSHBUTTON as u32,
            585,
            406,
            135,
            32,
            4,
        );
        DIALOG.with(|s| {
            *s.borrow_mut() = Some(Dialog {
                window,
                preview,
                side,
                offset,
                local,
                remote,
                placement,
                done: false,
                result: None,
                drag: None,
                scale: 1.0,
            })
        });
        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(window, SW_SHOW);
        let mut msg = MSG::default();
        while !DIALOG.with(|s| s.borrow().as_ref().is_none_or(|s| s.done))
            && GetMessageW(&mut msg, None, 0, 0).0 > 0
        {
            if !IsDialogMessageW(window, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let result = DIALOG.with(|s| s.borrow_mut().take().and_then(|s| s.result));
        let _ = DestroyWindow(window);
        let _ = EnableWindow(owner, true);
        let _ = SetForegroundWindow(owner);
        result
    }
}
#[allow(clippy::too_many_arguments)]
pub unsafe fn control(
    parent: HWND,
    class: &str,
    label: &str,
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    id: usize,
) -> HWND {
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(wide(class).as_ptr()),
            PCWSTR(wide(label).as_ptr()),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(style),
            x,
            y,
            w,
            h,
            Some(parent),
            Some(HMENU(id as *mut _)),
            Some(module()),
            None,
        )
        .unwrap_or_default()
    }
}
unsafe extern "system" fn proc(w: HWND, m: u32, p: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if m == WM_CLOSE {
            DIALOG.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.done = true
                }
            });
            return LRESULT(0);
        }
        if m == WM_COMMAND {
            DIALOG.with(|s|{let Ok(mut b)=s.try_borrow_mut()else{return};let Some(s)=b.as_mut()else{return};match p.0&0xffff{1|2=>{s.placement.right=SendMessageW(s.side,CB_GETCURSEL,None,None).0==0;if let Ok(value)=text(s.offset).parse::<i32>(){s.placement.offset=value.clamp(-20000,20000);}let _=InvalidateRect(Some(s.preview),None,true);},3=>s.done=true,4=>{if Layout::new(s.local.clone(),s.remote.clone(),s.placement).is_ok_and(|l|l.shared()){s.result=Some(s.placement);s.done=true;}else{MessageBoxW(Some(s.window),w!("Les bords des deux PC doivent se toucher sur une hauteur commune."),w!("Disposition"),MB_OK|MB_ICONINFORMATION);}},_=>()}});
            return LRESULT(0);
        }
        DefWindowProcW(w, m, p, l)
    }
}
unsafe extern "system" fn preview_proc(w: HWND, m: u32, p: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        match m {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(w, &mut ps);
                DIALOG.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        let mut rect = RECT::default();
                        let _ = GetClientRect(w, &mut rect);
                        let brush = CreateSolidBrush(COLORREF(0xf7f3ee));
                        FillRect(dc, &rect, brush);
                        let _ = DeleteObject(brush.into());
                        if let Ok(layout) =
                            Layout::new(s.local.clone(), s.remote.clone(), s.placement)
                        {
                            let all = layout
                                .local_screens
                                .iter()
                                .chain(layout.remote_screens.iter())
                                .collect::<Vec<_>>();
                            let minx = all.iter().map(|m| m.x).min().unwrap_or(0);
                            let miny = all.iter().map(|m| m.y).min().unwrap_or(0);
                            let maxx = all.iter().map(|m| m.right()).max().unwrap_or(1);
                            let maxy = all.iter().map(|m| m.bottom()).max().unwrap_or(1);
                            s.scale = (650.0 / (maxx - minx).max(1) as f64)
                                .min(180.0 / (maxy - miny).max(1) as f64);
                            for (remote, monitors) in [
                                (false, &layout.local_screens),
                                (true, &layout.remote_screens),
                            ] {
                                for (i, m) in monitors.iter().enumerate() {
                                    let mut r = RECT {
                                        left: 25 + ((m.x - minx) as f64 * s.scale) as i32,
                                        top: 30 + ((m.y - miny) as f64 * s.scale) as i32,
                                        right: 25 + ((m.right() - minx) as f64 * s.scale) as i32,
                                        bottom: 30 + ((m.bottom() - miny) as f64 * s.scale) as i32,
                                    };
                                    let brush = CreateSolidBrush(COLORREF(if remote {
                                        0xe9c07c
                                    } else {
                                        0xd2d2d2
                                    }));
                                    FillRect(dc, &r, brush);
                                    let _ = DeleteObject(brush.into());
                                    let border = CreateSolidBrush(COLORREF(0x605040));
                                    FrameRect(dc, &r, border);
                                    let _ = DeleteObject(border.into());
                                    let label = format!(
                                        "{} · {}",
                                        if remote { "Autre PC" } else { "Ce PC" },
                                        i + 1
                                    );
                                    let mut chars = wide(&label);
                                    DrawTextW(
                                        dc,
                                        &mut chars,
                                        &mut r,
                                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                                    );
                                }
                            }
                        }
                    }
                });
                let _ = EndPaint(w, &ps);
                return LRESULT(0);
            }
            WM_LBUTTONDOWN => {
                let y = (l.0 as u32 >> 16) as i16 as i32;
                DIALOG.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        s.drag = Some((y, s.placement.offset));
                    }
                });
                SetCapture(w);
                return LRESULT(0);
            }
            WM_MOUSEMOVE => {
                if p.0 & 1 != 0 {
                    let y = (l.0 as u32 >> 16) as i16 as i32;
                    DIALOG.with(|s| {
                        if let Some(s) = s.borrow_mut().as_mut() {
                            if let Some((start, offset)) = s.drag {
                                s.placement.offset = (offset
                                    + ((y - start) as f64 / s.scale) as i32)
                                    .clamp(-20000, 20000);
                                set_text(s.offset, &s.placement.offset.to_string());
                                let _ = InvalidateRect(Some(w), None, true);
                            }
                        }
                    });
                }
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                DIALOG.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        s.drag = None
                    }
                });
                let _ = ReleaseCapture();
                return LRESULT(0);
            }
            _ => (),
        }
        DefWindowProcW(w, m, p, l)
    }
}
