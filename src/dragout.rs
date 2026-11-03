//! Mac'ten sürüklenip Mac penceresinin dışına çıkarılan dosyalar için Windows'ta gerçek bir
//! sürükle-bırak başlatır: kullanıcı fareyi bıraktığı yere (Masaüstü, Explorer klasörü, bir
//! uygulama) dosyalar kopyalanır. Veri nesnesini Kabuk üretir (SHCreateDataObject), biz yalnız
//! sürükleme kaynağını (IDropSource) sağlarız.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use windows::core::{implement, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx, IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    IDropSource, IDropSource_Impl, IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, CF_HDROP, DROPEFFECT,
    DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MK_MBUTTON, MK_RBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{DragQueryFileW, ILFindLastID, SHCreateDataObject, SHDoDragDrop, SHParseDisplayName, HDROP};

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

/// Aynı klasördeki öğeleri sürükler; fare bırakılınca döner. Bırakıldıysa (iptal değil) true. Kabuk
/// sürükleme görüntüsünü (simge ve ad) kendisi çizer.
pub fn drag(hwnd: HWND, items: &[PathBuf]) -> bool {
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
        ACTIVE.with(|a| a.set(true));
        let result = SHDoDragDrop(hwnd, &data, &source, DROPEFFECT_COPY);
        ACTIVE.with(|a| a.set(false));
        result.is_ok_and(|e| e != DROPEFFECT_NONE)
    }
}

// ---------------------------------------------------------------------------------------------
// Windows'tan Mac penceresine bırakma (OLE hedefi): sürüklenirken Mac'te dosyanın simgesi ve adı
// gösterilir; Windows kendi sürükleme görüntüsünü çizmez (aynı şey iki kez görünmesin).

#[implement(IDropTarget)]
struct Target {
    paths: RefCell<Vec<PathBuf>>,
    /// İlk öğenin adı ve klasör olup olmadığı: girişte bir kez okunur (her fare hareketinde dosya
    /// sistemine sorulmaz; ağ paylaşımında arayüz takılmasın).
    first: RefCell<(String, bool)>,
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, data: Option<&IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        // Mac'ten gelen dosya yeniden Mac penceresine sürüklenirse kabul edilmez.
        let paths = if active() || !copy_allowed(effect) { Vec::new() } else { data.map(dropped_files).unwrap_or_default() };
        let first = paths.first().map(|p| {
            (p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), p.is_dir())
        });
        let ok = match &first {
            Some((name, folder)) => crate::macview::drop_hover(pt.x, pt.y, paths.len(), *folder, name, true),
            None => false,
        };
        *self.first.borrow_mut() = first.unwrap_or_default();
        *self.paths.borrow_mut() = if ok { paths } else { Vec::new() };
        set_effect(effect, ok);
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        let count = self.paths.borrow().len();
        let (name, folder) = self.first.borrow().clone();
        let ok = count > 0 && copy_allowed(effect) && crate::macview::drop_hover(pt.x, pt.y, count, folder, &name, false);
        set_effect(effect, ok);
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        if !std::mem::take(&mut *self.paths.borrow_mut()).is_empty() {
            crate::macview::drop_leave();
        }
        Ok(())
    }

    fn Drop(&self, _data: Option<&IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        let paths = std::mem::take(&mut *self.paths.borrow_mut());
        if paths.is_empty() {
            set_effect(effect, false);
            return Ok(());
        }
        crate::macview::drop_leave();
        let ok = crate::macview::drop_files(pt.x, pt.y, paths);
        set_effect(effect, ok);
        Ok(())
    }
}

/// Kaynak kopyalamaya izin veriyor mu (yalnız taşıma/bağlantı veren kaynaktan kopya istenmez).
fn copy_allowed(effect: *mut DROPEFFECT) -> bool {
    !effect.is_null() && unsafe { (*effect).0 & DROPEFFECT_COPY.0 != 0 }
}

fn set_effect(effect: *mut DROPEFFECT, ok: bool) {
    if !effect.is_null() {
        unsafe { *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
    }
}

/// Sürüklenen veri nesnesindeki dosya yolları (CF_HDROP).
fn dropped_files(data: &IDataObject) -> Vec<PathBuf> {
    let format = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    unsafe {
        let Ok(mut medium) = data.GetData(&format) else { return Vec::new() };
        let drop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(drop, u32::MAX, None);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(drop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let n = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
            paths.push(PathBuf::from(String::from_utf16_lossy(&buf[..n])));
        }
        ReleaseStgMedium(&mut medium);
        paths
    }
}

/// Mac penceresini OLE bırakma hedefi yapar.
pub fn register_target(hwnd: HWND) {
    let target: IDropTarget = Target { paths: RefCell::new(Vec::new()), first: RefCell::new((String::new(), false)) }.into();
    if let Err(e) = unsafe { RegisterDragDrop(hwnd, &target) } {
        util::log(&format!("Mac penceresi bırakma hedefi olamadı: {e}"));
    }
}
