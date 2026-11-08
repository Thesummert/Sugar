use std::path::Path;

cpp::cpp! {{
    #include <QtCore/QMimeData>
    #include <QtCore/QUrl>
    #include <QtGui/QDrag>
    #include <QtWidgets/QApplication>
    #include <QtWidgets/QWidget>
}}

#[derive(Debug, PartialEq, Eq)]
pub enum DragResult {
    Copied,
    Cancelled,
    Released,
}

// Call on the UI thread, after background extraction has finished. The target
// file manager owns the copy and its usual conflict dialog. Only CopyAction is
// offered, so neither a modifier key nor the target can move archive contents.
pub fn start(path: &Path) -> DragResult {
    let path = qttypes::QString::from(path.to_string_lossy().as_ref());
    // SAFETY: Qt is initialized by Slint and this runs on its UI thread. QDrag
    // owns the MIME data; the source widget outlives this nested event loop.
    let result = unsafe {
        cpp::cpp!([path as "QString"] -> i32 as "int" {
            auto source = QApplication::activeWindow();
            if (!source || !(QApplication::mouseButtons() & Qt::LeftButton))
                return 0;
            auto drag = new QDrag(source);
            auto mime = new QMimeData;
            mime->setUrls({QUrl::fromLocalFile(path)});
            drag->setMimeData(mime);
            return drag->exec(Qt::CopyAction, Qt::CopyAction) == Qt::CopyAction ? 1 : 2;
        })
    };
    match result {
        1 => DragResult::Copied,
        2 => DragResult::Cancelled,
        _ => DragResult::Released,
    }
}
