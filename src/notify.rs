//! macOS's attention: the dock for a bell, Notification Center for a program's notification.
//!
//! Notifications need rtty in an app bundle. Without a bundle identifier,
//! `UNUserNotificationCenter` raises (`bundleProxyForCurrentProcess is nil`) and takes the process
//! down, and the deprecated `NSUserNotificationCenter` accepts a notification and drops it
//! (probed on macOS 26 with a plain executable and with the same one in a minimal `.app`). So an
//! unbundled rtty, `cargo run` included, says so once and posts nothing.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use block2::RcBlock;
use futures::channel::mpsc::UnboundedSender;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, MainThreadMarker, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSRequestUserAttentionType};
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use crate::layout::PaneId;

/// Bounces rtty's dock icon once.
pub fn request_attention() {
    if let Some(main_thread) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(main_thread);
        app.requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
    }
}

/// The identifier a notification from pane `pane` carries, and the pane one names.
fn identifier(pane: PaneId) -> String {
    format!("rtty-pane-{pane}")
}

fn pane_of(identifier: &str) -> Option<PaneId> {
    identifier.strip_prefix("rtty-pane-")?.parse().ok()
}

define_class!(
    /// Shows rtty's notifications while rtty is frontmost too, and sends the pane of a clicked
    /// one to the app.
    #[unsafe(super(NSObject))]
    #[name = "RttyNotificationDelegate"]
    #[ivars = UnboundedSender<PaneId>]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion.call((
                UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List,
            ));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            let identifier = response.notification().request().identifier().to_string();
            if let Some(pane) = pane_of(&identifier) {
                let _ = self.ivars().unbounded_send(pane);
            }
            completion.call(());
        }
    }
);

/// rtty's notification centre, when rtty runs from a bundle.
struct Center {
    center: Retained<UNUserNotificationCenter>,
    /// Kept alive: the centre holds its delegate weakly.
    _delegate: Retained<Delegate>,
}

// SAFETY: UNUserNotificationCenter is documented thread-safe; the delegate is only handed to it.
unsafe impl Send for Center {}
unsafe impl Sync for Center {}

static CENTER: OnceLock<Option<Center>> = OnceLock::new();
static AUTHORIZATION_ASKED: AtomicBool = AtomicBool::new(false);
static UNBUNDLED_SAID: AtomicBool = AtomicBool::new(false);

/// Whether rtty runs from an app bundle, which notifications need.
pub fn bundled() -> bool {
    NSBundle::mainBundle().bundleIdentifier().is_some()
}

/// Sets notifications up for a bundled rtty: a clicked notification's pane goes to `clicks`.
/// Does nothing unbundled.
pub fn init(clicks: UnboundedSender<PaneId>) {
    CENTER.get_or_init(|| {
        bundled().then(|| {
            let center = UNUserNotificationCenter::currentNotificationCenter();
            let delegate = Delegate::alloc().set_ivars(clicks);
            let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };
            center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            Center { center, _delegate: delegate }
        })
    });
}

/// Posts a notification from pane `pane`, asking for permission the first time. Unbundled, says
/// once on stderr why nothing shows. Whether it was posted.
pub fn post(pane: PaneId, title: Option<&str>, body: Option<&str>) -> bool {
    let Some(Some(Center { center, .. })) = CENTER.get() else {
        if !UNBUNDLED_SAID.swap(true, Ordering::Relaxed) {
            eprintln!(
                "rtty: notifications need rtty in an app bundle (see the README); \
                 not shown: {}",
                [title, body].into_iter().flatten().collect::<Vec<_>>().join(": ")
            );
        }
        return false;
    };
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title.unwrap_or("rtty")));
    content.setBody(&NSString::from_str(body.unwrap_or_default()));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&identifier(pane)),
        &content,
        None,
    );
    let add = RcBlock::new(move |_granted: Bool, _error: *mut NSError| {
        if let Some(Some(Center { center, .. })) = CENTER.get() {
            center.addNotificationRequest_withCompletionHandler(&request, None);
        }
    });
    if AUTHORIZATION_ASKED.swap(true, Ordering::Relaxed) {
        add.call((Bool::YES, std::ptr::null_mut()));
    } else {
        let options = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
        center.requestAuthorizationWithOptions_completionHandler(options, &add);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notification_names_its_pane() {
        assert_eq!(pane_of(&identifier(17)), Some(17));
        assert_eq!(pane_of("rtty-pane-x"), None);
        assert_eq!(pane_of("other-17"), None);
    }

    /// The test binary has no bundle: posting must say so, not reach the centre (which would
    /// abort the process).
    #[test]
    fn unbundled_nothing_is_posted_and_nothing_aborts() {
        assert!(!bundled());
        let (clicks, _) = futures::channel::mpsc::unbounded();
        init(clicks);
        assert!(!post(1, Some("title"), Some("body")));
        assert!(!post(1, None, Some("again")));
    }
}
