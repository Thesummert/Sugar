use objc2::{
    declare_class, msg_send, msg_send_id, mutability, rc::Retained, sel, ClassType, DeclaredClass,
};
use objc2_foundation::{
    MainThreadMarker, NSAppleEventDescriptor, NSAppleEventManager, NSObject, NSObjectProtocol,
};
use std::{cell::RefCell, collections::VecDeque, path::PathBuf, time::Duration};

thread_local! {
    static OPEN_FILES: RefCell<VecDeque<PathBuf>> = RefCell::new(VecDeque::new());
}

declare_class!(
    pub struct OpenEventHandler;
    unsafe impl ClassType for OpenEventHandler {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "SugarArchiveOpenEventHandler";
    }
    impl DeclaredClass for OpenEventHandler {}
    unsafe impl NSObjectProtocol for OpenEventHandler {}
    unsafe impl OpenEventHandler {
        #[method(handleOpenEvent:withReplyEvent:)]
        fn handle_open_event(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            // The direct object contains the list of file URLs sent by Finder.
            let list: Option<Retained<NSAppleEventDescriptor>> = unsafe {
                msg_send_id![event, paramDescriptorForKeyword: u32::from_be_bytes(*b"----")]
            };
            if let Some(list) = list {
                OPEN_FILES.with(|files| {
                    let mut files = files.borrow_mut();
                    for index in 1..=unsafe { list.numberOfItems() } {
                        let path = unsafe { list.descriptorAtIndex(index) }
                            .and_then(|item| unsafe { item.fileURLValue() })
                            .and_then(|url| unsafe { url.path() });
                        if let Some(path) = path {
                            files.push_back(PathBuf::from(path.to_string()));
                        }
                    }
                });
            }
        }
    }
);

pub fn receive_open_files(
    mut open: impl FnMut(PathBuf) -> bool + 'static,
) -> (Retained<OpenEventHandler>, slint::Timer) {
    let mtm = MainThreadMarker::new().expect("macOS application must run on the main thread");
    let handler: Retained<OpenEventHandler> =
        unsafe { msg_send_id![super(mtm.alloc().set_ivars(())), init] };
    // Register only the open-document Apple event, leaving Winit's application delegate intact.
    unsafe {
        let manager = NSAppleEventManager::sharedAppleEventManager();
        let _: () = msg_send![&*manager,
            setEventHandler: &*handler
            andSelector: sel!(handleOpenEvent:withReplyEvent:)
            forEventClass: u32::from_be_bytes(*b"aevt")
            andEventID: u32::from_be_bytes(*b"odoc")
        ];
    }
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(200),
        move || {
            let path = OPEN_FILES.with(|files| files.borrow_mut().pop_front());
            if let Some(path) = path {
                if !open(path.clone()) {
                    OPEN_FILES.with(|files| files.borrow_mut().push_front(path));
                }
            }
        },
    );
    (handler, timer)
}
