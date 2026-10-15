//! AppKit front end: renders `bar::menu::Menu` into an NSMenu behind an
//! NSStatusItem. All menu content and click semantics live in the model; this
//! file only draws and routes.

use crate::macos_agent as agent;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSCellImagePosition, NSColor,
    NSControlStateValueOff, NSControlStateValueOn, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSImage, NSImageSymbolConfiguration, NSMenu, NSMenuDelegate,
    NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{
    NSArray, NSMutableAttributedString, NSObject, NSObjectProtocol, NSRange, NSRunLoop,
    NSRunLoopCommonModes, NSString, NSTimer, ns_string,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use tachi_noti::bar::menu::{self, Action, Dispatched, Item};
use tachi_noti::{bar, state};

/// What a tagged menu item does when clicked. Session rows also carry their
/// identity so live label updates can never attach to the wrong row.
struct Slot {
    action: Action,
    /// (session_id, group_key) for session rows.
    session: Option<(String, String)>,
}

pub struct Ivars {
    status_item: RefCell<Option<Retained<NSStatusItem>>>,
    menu: RefCell<Option<Retained<NSMenu>>>,
    /// Indexed by menu item tag.
    slots: RefCell<Vec<Slot>>,
    menu_open: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TachiBarController"]
    #[ivars = Ivars]
    pub struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    impl Controller {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            self.refresh();
        }

        #[unsafe(method(menuAction:))]
        fn menu_action(&self, sender: &NSMenuItem) {
            let idx = sender.tag() as usize;
            let action = self.ivars().slots.borrow().get(idx).map(|s| s.action.clone());
            let Some(action) = action else { return };
            match menu::dispatch_shared(&action) {
                Dispatched::Done => {}
                Dispatched::ToggleLaunchAtLogin => {
                    if agent::is_installed() { agent::uninstall() } else { agent::install() }
                }
                Dispatched::Restart => crate::restart(),
                Dispatched::Quit => NSApplication::sharedApplication(self.mtm()).terminate(None),
            }
        }
    }

    unsafe impl NSMenuDelegate for Controller {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            self.rebuild(menu);
        }

        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            self.ivars().menu_open.set(true);
        }

        #[unsafe(method(menuDidClose:))]
        fn menu_did_close(&self, _menu: &NSMenu) {
            self.ivars().menu_open.set(false);
        }
    }
);

impl Controller {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            status_item: RefCell::new(None),
            menu: RefCell::new(None),
            slots: RefCell::new(Vec::new()),
            menu_open: Cell::new(false),
        });
        unsafe { msg_send![super(this), init] }
    }

    /// Render "●1 ●2" next to the dog icon, dots tinted per status; a red
    /// ⚠ appears when usage crosses the configured threshold.
    fn set_title_segments(&self, segments: &[(usize, state::Status)], usage_alert: bool) {
        let Some(item) = self.ivars().status_item.borrow().clone() else { return };
        let Some(button) = item.button(self.mtm()) else { return };

        let mut plain = String::new();
        let mut dot_ranges: Vec<(usize, state::Status)> = Vec::new();
        for (i, (count, status)) in segments.iter().enumerate() {
            if i > 0 {
                plain.push(' ');
            }
            dot_ranges.push((plain.encode_utf16().count(), *status));
            plain.push('\u{25CF}');
            plain.push_str(&count.to_string());
        }
        if !plain.is_empty() {
            plain.insert(0, ' '); // breathing room after the dog icon
            for r in &mut dot_ranges {
                r.0 += 1;
            }
        }

        let mut alert_loc = None;
        if usage_alert {
            alert_loc = Some(plain.encode_utf16().count() + 1);
            plain.push_str(" \u{26A0}\u{FE0E}"); // ⚠ text-style, tinted red below
        }

        let attr = NSMutableAttributedString::from_nsstring(&NSString::from_str(&plain));
        let full = NSRange::new(0, plain.encode_utf16().count());
        unsafe {
            attr.addAttribute_value_range(NSForegroundColorAttributeName, &NSColor::labelColor(), full);
            attr.addAttribute_value_range(NSFontAttributeName, &NSFont::menuBarFontOfSize(0.0), full);
            for (loc, status) in dot_ranges {
                attr.addAttribute_value_range(
                    NSForegroundColorAttributeName,
                    &status_color(status),
                    NSRange::new(loc, 1),
                );
            }
            if let Some(loc) = alert_loc {
                attr.addAttribute_value_range(
                    NSForegroundColorAttributeName,
                    &NSColor::systemRedColor(),
                    NSRange::new(loc, 2),
                );
            }
            button.setAttributedTitle(&attr);
        }
    }

    /// 2s heartbeat: title always; open-menu row labels by session identity.
    fn refresh(&self) {
        let now = state::now_epoch();
        let states = state::reap_dead(state::load_all());
        let (segments, alert) = menu::title(&states, now);
        self.set_title_segments(&segments, alert);

        if !self.ivars().menu_open.get() {
            return;
        }
        let by_id: HashMap<String, state::SessionState> =
            states.into_iter().map(|s| (s.session_id.clone(), s)).collect();
        let menu = self.ivars().menu.borrow().clone();
        let Some(menu) = menu else { return };
        let slots = self.ivars().slots.borrow();
        for item in menu.itemArray() {
            if item.action() != Some(sel!(menuAction:)) {
                continue;
            }
            let Some(Slot { session: Some((session_id, group_key)), .. }) = slots.get(item.tag() as usize) else {
                continue;
            };
            let Some(s) = by_id.get(session_id) else { continue };
            let row = bar::build_row(s, group_key, now);
            item.setTitle(&NSString::from_str(&row.label));
            item.setImage(status_image(row.status).as_deref());
            item.setToolTip(row.tooltip.map(|t| NSString::from_str(&t)).as_deref());
        }
    }

    fn rebuild(&self, menu: &NSMenu) {
        menu.removeAllItems();
        let now = state::now_epoch();
        let states = state::reap_dead(state::load_all());
        let model = menu::build(states, now, &menu::Env { launch_at_login: agent::is_installed() });
        let target: &AnyObject = self;
        let mut slots: Vec<Slot> = Vec::new();
        render(self.mtm(), target, &model.items, menu, &mut slots);
        *self.ivars().slots.borrow_mut() = slots;
    }
}

/// Wire an item to the controller's single `menuAction:` selector, tagged
/// with its slot index.
fn bind(item: &NSMenuItem, target: &AnyObject, slots: &mut Vec<Slot>, slot: Slot) {
    unsafe {
        item.setTarget(Some(target));
        item.setAction(Some(sel!(menuAction:)));
    }
    item.setTag(slots.len() as isize);
    slots.push(slot);
}

fn render(mtm: MainThreadMarker, target: &AnyObject, items: &[Item], menu: &NSMenu, slots: &mut Vec<Slot>) {
    for entry in items {
        match entry {
            Item::Header(title) => menu.addItem(&section_header(mtm, title)),
            Item::Text(text) => menu.addItem(&disabled_item(mtm, text)),
            Item::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            Item::Session(s) => {
                let item = NSMenuItem::new(mtm);
                item.setTitle(&NSString::from_str(&s.row.label));
                item.setImage(status_image(s.row.status).as_deref());
                item.setToolTip(s.row.tooltip.as_deref().map(NSString::from_str).as_deref());
                match &s.action {
                    Some(action) => bind(
                        &item,
                        target,
                        slots,
                        Slot {
                            action: action.clone(),
                            session: Some((s.session_id.clone(), s.group_key.clone())),
                        },
                    ),
                    None => item.setEnabled(false),
                }
                menu.addItem(&item);
            }
            Item::Submenu { title, items } => {
                let parent = NSMenuItem::new(mtm);
                parent.setTitle(&NSString::from_str(title));
                let sub = NSMenu::new(mtm);
                sub.setAutoenablesItems(false);
                render(mtm, target, items, &sub, slots);
                parent.setSubmenu(Some(&sub));
                menu.addItem(&parent);
            }
            Item::Choice { title, checked, action } => {
                let item = NSMenuItem::new(mtm);
                item.setTitle(&NSString::from_str(title));
                item.setState(if *checked { NSControlStateValueOn } else { NSControlStateValueOff });
                bind(&item, target, slots, Slot { action: action.clone(), session: None });
                menu.addItem(&item);
            }
            Item::Command { title, action, key } => {
                let item = NSMenuItem::new(mtm);
                item.setTitle(&NSString::from_str(title));
                if let Some(k) = key {
                    item.setKeyEquivalent(&NSString::from_str(&k.to_string()));
                }
                bind(&item, target, slots, Slot { action: action.clone(), session: None });
                menu.addItem(&item);
            }
        }
    }
}

fn status_color(status: state::Status) -> objc2::rc::Retained<NSColor> {
    match status {
        state::Status::Running => NSColor::systemGreenColor(),
        state::Status::Waiting => NSColor::systemYellowColor(),
        state::Status::Idle => NSColor::tertiaryLabelColor(),
    }
}

/// Small tinted circle.fill for a menu row.
fn status_image(status: state::Status) -> Option<Retained<NSImage>> {
    let img =
        NSImage::imageWithSystemSymbolName_accessibilityDescription(ns_string!("circle.fill"), None)?;
    let palette = NSImageSymbolConfiguration::configurationWithPaletteColors(
        &NSArray::from_retained_slice(&[status_color(status)]),
    );
    let size = NSImageSymbolConfiguration::configurationWithPointSize_weight_scale(
        9.0,
        0.0, // NSFontWeightRegular
        objc2_app_kit::NSImageSymbolScale::Small,
    );
    let combined = size.configurationByApplyingConfiguration(&palette);
    img.imageWithSymbolConfiguration(&combined)
}

/// Tachi's own silhouette (drawn by Steven), embedded so the binary stays
/// self-contained. Template rendering follows the menu bar's appearance.
const TACHI_TEMPLATE: &[u8] = include_bytes!("../../../assets/tachi-menubar-template.png");

fn dog_image() -> Option<Retained<NSImage>> {
    let data = objc2_foundation::NSData::with_bytes(TACHI_TEMPLATE);
    if let Some(img) = NSImage::initWithData(NSImage::alloc(), &data) {
        img.setTemplate(true);
        img.setSize(objc2_foundation::NSSize::new(18.0, 18.0));
        return Some(img);
    }
    // Defensive fallback: the generic system dog.
    let img =
        NSImage::imageWithSystemSymbolName_accessibilityDescription(ns_string!("dog.fill"), None)?;
    img.setTemplate(true);
    Some(img)
}

fn disabled_item(mtm: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    item.setEnabled(false);
    item
}

fn section_header(mtm: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
    // sectionHeaderWithTitle is macOS 14+; all supported targets have it.
    NSMenuItem::sectionHeaderWithTitle(&NSString::from_str(title), mtm)
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("tachi-bar must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let controller = Controller::new(mtm);

    let status_bar = NSStatusBar::systemStatusBar();
    let item = status_bar.statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        button.setImage(dog_image().as_deref());
        button.setImagePosition(NSCellImagePosition::ImageLeft);
    }
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    menu.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    item.setMenu(Some(&menu));
    *controller.ivars().status_item.borrow_mut() = Some(item);
    *controller.ivars().menu.borrow_mut() = Some(menu);

    controller.refresh();

    let timer = unsafe {
        NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
            2.0,
            &controller,
            sel!(tick:),
            None,
            true,
        )
    };
    // CommonModes keeps the timer firing while the menu is open (menu
    // tracking runs the loop in NSEventTrackingRunLoopMode).
    unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };

    app.run();
}
