use windows::Win32::{
    Foundation::*,
    System::{StationsAndDesktops::*, Threading::GetCurrentThreadId},
};

// Query only: never switch desktops, elevate, or try to inject on the secure one.
pub fn available() -> bool {
    unsafe {
        let Ok(input) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS)
        else {
            return false;
        };
        let matches = GetThreadDesktop(GetCurrentThreadId())
            .ok()
            .is_some_and(|own| {
                let name = |desktop: HDESK| {
                    let mut buffer = [0u16; 256];
                    GetUserObjectInformationW(
                        HANDLE(desktop.0),
                        UOI_NAME,
                        Some(buffer.as_mut_ptr().cast()),
                        std::mem::size_of_val(&buffer) as u32,
                        None,
                    )
                    .ok()
                    .map(|_| buffer)
                };
                name(own).zip(name(input)).is_some_and(|(a, b)| a == b)
            });
        let _ = CloseDesktop(input);
        matches
    }
}
