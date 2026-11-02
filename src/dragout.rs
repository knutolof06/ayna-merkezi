//! Mac'ten sürüklenip Mac penceresinin dışına çıkarılan dosyalar için Windows'ta gerçek bir
//! sürükle-bırak başlatır: kullanıcı fareyi bıraktığı yere (Masaüstü, Explorer klasörü, bir
//! uygulama) dosyalar kopyalanır. Veri nesnesini Kabuk üretir (SHCreateDataObject), biz yalnız
//! sürükleme kaynağını (IDropSource) sağlarız.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use windows::core::{implement, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx, IDataObject};
use windows::Win32::System::Ole::{DoDragDrop, IDropSource, IDropSource_Impl, DROPEFFECT, DROPEFFECT_COPY};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MK_MBUTTON, MK_RBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{ILFindLastID, SHCreateDataObject, SHParseDisplayName};

use crate::util;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// Sürükleme sürüyor mu (bırakma Mac penceresine düşerse dosyalar geri gönderilmesin).
pub fn active() -> bool {
    ACTIVE.with(|a| a.get())
}

#[implement(IDropSource)]
struct Source;

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> windows::core::HRESULT {
        if escape.as_bool() || keys.0 & (MK_RBUTTON.0 | MK_MBUTTON.0) != 0 {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> windows::core::HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Kabuk öğe kimliği; bırakılınca serbest kalır.
struct Pidl(*mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
        }
    }
}

fn pidl(path: &Path) -> Option<Pidl> {
    let w = util::wide(&path.to_string_lossy());
    let mut p: *mut ITEMIDLIST = std::ptr::null_mut();
    unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None::<&IBindCtx>, &mut p, 0, None).ok()? };
    (!p.is_null()).then_some(Pidl(p))
}

/// Aynı klasördeki öğeleri sürükler; fare bırakılınca döner. Bırakıldıysa (iptal değil) true.
pub fn drag(items: &[PathBuf]) -> bool {
    let Some(parent) = items.first().and_then(|p| p.parent()) else { return false };
    let Some(folder) = pidl(parent) else { return false };
    let abs: Vec<Pidl> = items.iter().filter_map(|p| pidl(p)).collect();
    if abs.is_empty() {
        return false;
    }
    unsafe {
        let children: Vec<*const ITEMIDLIST> = abs.iter().map(|p| ILFindLastID(p.0) as *const ITEMIDLIST).collect();
        let data: windows::core::Result<IDataObject> = SHCreateDataObject(Some(folder.0), Some(&children), None::<&IDataObject>);
        let Ok(data) = data else { return false };
        let source: IDropSource = Source.into();
        let mut effect = DROPEFFECT::default();
        ACTIVE.with(|a| a.set(true));
        let result = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
        ACTIVE.with(|a| a.set(false));
        result == DRAGDROP_S_DROP
    }
}
