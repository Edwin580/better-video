//! The standard Windows folder picker (`IFileOpenDialog` in folder mode).

use std::path::{Path, PathBuf};

use windows::{
    core::{HSTRING, PCWSTR},
    Win32::{
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_APARTMENTTHREADED,
        },
        UI::Shell::{
            FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
            FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
        },
    },
};

/// Show the dialog starting at `start`; `None` if cancelled.
pub fn choose(start: &Path) -> Option<PathBuf> {
    unsafe {
        // Balanced below; fine to call on a thread that already has COM.
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let picked = pick(start);
        if init {
            CoUninitialize();
        }
        picked
    }
}

unsafe fn pick(start: &Path) -> Option<PathBuf> {
    let dialog: IFileOpenDialog =
        CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
    let options = dialog.GetOptions().ok()?;
    dialog
        .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
        .ok()?;
    let title = HSTRING::from("Save better-video recordings in");
    let _ = dialog.SetTitle(PCWSTR(title.as_ptr()));
    let folder = HSTRING::from(start.as_os_str());
    if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(folder.as_ptr()), None)
    {
        let _ = dialog.SetFolder(&item);
    }
    // Err when cancelled.
    dialog.Show(None).ok()?;
    let item = dialog.GetResult().ok()?;
    let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
    let path = name.to_string().ok();
    CoTaskMemFree(Some(name.0 as *const _));
    path.map(PathBuf::from)
}
