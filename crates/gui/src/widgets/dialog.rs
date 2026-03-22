//! Async file dialog helpers.
//! rfd on macOS panics if called synchronously inside an egui paint callback.
//! We spawn the dialog on a background thread and poll the result next frame.

use crate::state::FileInfo;
use std::path::PathBuf;
use std::sync::mpsc;

/// Identifies which file slot a pending dialog result should fill.
#[derive(Clone, Debug)]
pub enum DialogTarget {
    File(egui::Id),
    Dir(egui::Id),
    SaveFile { src: PathBuf },
}

/// Pending dialog result receiver — stored on App struct.
pub struct PendingDialog {
    pub target: DialogTarget,
    pub rx: mpsc::Receiver<Option<PathBuf>>,
}

/// Completed dialog result ready to be applied.
pub enum DialogResult {
    File(egui::Id, Option<FileInfo>),
    Dir(egui::Id, PathBuf),
    SaveCopy { src: PathBuf, dest: PathBuf },
}

/// Spawn a file-pick dialog on a background thread.
pub fn spawn_file_dialog(
    target: DialogTarget,
    filters: Vec<(String, Vec<String>)>,
) -> PendingDialog {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut dialog = rfd::FileDialog::new();
        for (name, exts) in &filters {
            let ext_refs: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();
            dialog = dialog.add_filter(name, &ext_refs);
        }
        tx.send(dialog.pick_file()).ok();
    });
    PendingDialog { target, rx }
}

/// Spawn a directory-pick dialog on a background thread.
pub fn spawn_dir_dialog(target: DialogTarget) -> PendingDialog {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        tx.send(rfd::FileDialog::new().pick_folder()).ok();
    });
    PendingDialog { target, rx }
}

/// Spawn a save-file dialog on a background thread.
pub fn spawn_save_file_dialog(name: String, src: PathBuf) -> PendingDialog {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        tx.send(rfd::FileDialog::new().set_file_name(&name).save_file())
            .ok();
    });
    PendingDialog {
        target: DialogTarget::SaveFile { src },
        rx,
    }
}

/// Poll all pending dialogs. Returns completed results; unfinished ones stay in the vec.
pub fn poll_dialogs(pending: &mut Vec<PendingDialog>) -> Vec<DialogResult> {
    let all: Vec<PendingDialog> = std::mem::take(pending);
    let mut results = Vec::new();
    for p in all {
        match p.rx.try_recv() {
            Ok(Some(path)) => match &p.target {
                DialogTarget::File(id) => {
                    results.push(DialogResult::File(*id, FileInfo::from_path(path)));
                }
                DialogTarget::Dir(id) => {
                    results.push(DialogResult::Dir(*id, path));
                }
                DialogTarget::SaveFile { src, .. } => {
                    results.push(DialogResult::SaveCopy {
                        src: src.clone(),
                        dest: path,
                    });
                }
            },
            Ok(None) => {}
            Err(mpsc::TryRecvError::Empty) => {
                pending.push(p);
            }
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
    }
    results
}
