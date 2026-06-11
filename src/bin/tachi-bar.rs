//! tachi-bar — menu bar monitor for live Claude Code sessions.
//! Reads the state files written by `tachi-noti hook`; no daemon, no IPC.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tachi-bar", version, about = "Menu bar monitor for Claude Code sessions")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Install a launchd agent so tachi-bar starts at login
    InstallAgent,
    /// Remove the launchd agent
    UninstallAgent,
}

fn main() {
    match Cli::parse().cmd {
        Some(Cmd::InstallAgent) => agent::install(),
        Some(Cmd::UninstallAgent) => agent::uninstall(),
        None => ui::run(),
    }
}

mod agent {
    use std::path::PathBuf;
    use std::process::Command;

    pub const LABEL: &str = "com.tachi-noti.bar";

    pub fn plist_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }

    fn plist(exe: &str) -> String {
        let exe = xml_escape(exe);
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{exe}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
</dict></plist>
"#
        )
    }

    fn uid() -> String {
        Command::new("/usr/bin/id")
            .arg("-u")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "501".into())
    }

    pub fn is_installed() -> bool {
        plist_path().exists()
    }

    pub fn install() {
        let exe = match std::env::current_exe().and_then(|p| p.canonicalize()) {
            Ok(p) => p.display().to_string(),
            Err(e) => {
                eprintln!("error: cannot resolve tachi-bar path: {e}");
                std::process::exit(1);
            }
        };
        let path = plist_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Unload a previous version first; failure is fine (not loaded).
        let _ = Command::new("/bin/launchctl")
            .args(["bootout", &format!("gui/{}/{LABEL}", uid())])
            .output();
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        if std::fs::write(&tmp, plist(&exe)).is_err() || std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            eprintln!("error: cannot write {}", path.display());
            std::process::exit(1);
        }
        let status = Command::new("/bin/launchctl")
            .args(["bootstrap", &format!("gui/{}", uid())])
            .arg(&path)
            .status();
        match status {
            Ok(s) if s.success() => println!("Installed launch agent {} ({})", LABEL, path.display()),
            _ => eprintln!("warning: wrote {} but launchctl bootstrap failed; it will load at next login", path.display()),
        }
    }

    pub fn uninstall() {
        let _ = Command::new("/bin/launchctl")
            .args(["bootout", &format!("gui/{}/{LABEL}", uid())])
            .output();
        let path = plist_path();
        if path.exists() {
            let _ = std::fs::remove_file(&path);
            println!("Removed launch agent {LABEL}");
        } else {
            println!("Launch agent not installed — nothing to do.");
        }
    }
}

mod ui {
    use crate::agent;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject, Sel};
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
    use tachi_noti::{bar, history, state};

    /// Click target for a menu row, kept session-identified so live label
    /// updates can never attach to the wrong row.
    #[derive(Clone)]
    struct Action {
        session_id: String,
        group_key: String,
        bundle_id: Option<String>,
        open_path: Option<String>,
    }

    pub struct Ivars {
        status_item: RefCell<Option<Retained<NSStatusItem>>>,
        menu: RefCell<Option<Retained<NSMenu>>>,
        actions: RefCell<Vec<Action>>,
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

            #[unsafe(method(focusSession:))]
            fn focus_session(&self, sender: &NSMenuItem) {
                let idx = sender.tag() as usize;
                if let Some(a) = self.ivars().actions.borrow().get(idx).cloned() {
                    if let Some(bid) = &a.bundle_id {
                        bar::focus(bid, a.open_path.as_deref());
                    }
                }
            }

            #[unsafe(method(toggleAgent:))]
            fn toggle_agent(&self, _sender: &NSMenuItem) {
                if agent::is_installed() { agent::uninstall() } else { agent::install() }
            }

            #[unsafe(method(quit:))]
            fn quit(&self, _sender: &NSMenuItem) {
                NSApplication::sharedApplication(self.mtm()).terminate(None);
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
                actions: RefCell::new(Vec::new()),
                menu_open: Cell::new(false),
            });
            unsafe { msg_send![super(this), init] }
        }

        /// Render "●1 ●2" next to the dog icon, dots tinted per status.
        fn set_title_segments(&self, segments: &[(usize, state::Status)]) {
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
                button.setAttributedTitle(&attr);
            }
        }

        /// 2s heartbeat: title always; open-menu row labels by session identity.
        fn refresh(&self) {
            let now = state::now_epoch();
            let states = state::load_all();
            let snap = bar::build_snapshot(states.clone(), now);
            self.set_title_segments(&bar::title_segments(&snap));

            if !self.ivars().menu_open.get() {
                return;
            }
            let by_id: HashMap<String, state::SessionState> =
                states.into_iter().map(|s| (s.session_id.clone(), s)).collect();
            let menu = self.ivars().menu.borrow().clone();
            let actions = self.ivars().actions.borrow().clone();
            let Some(menu) = menu else { return };
            for item in menu.itemArray() {
                if item.action() != Some(sel!(focusSession:)) {
                    continue;
                }
                let idx = item.tag() as usize;
                let Some(a) = actions.get(idx) else { continue };
                let Some(s) = by_id.get(&a.session_id) else { continue };
                let row = bar::build_row(s, &a.group_key, now);
                item.setTitle(&NSString::from_str(&row.label));
                item.setImage(status_image(row.status).as_deref());
                item.setToolTip(row.tooltip.map(|t| NSString::from_str(&t)).as_deref());
            }
        }

        fn rebuild(&self, menu: &NSMenu) {
            let mtm = self.mtm();
            menu.removeAllItems();
            let now = state::now_epoch();
            let states = state::load_all();
            let snap = bar::build_snapshot(states.clone(), now);
            let mut actions: Vec<Action> = Vec::new();

            if snap.groups.is_empty() {
                menu.addItem(&disabled_item(mtm, "No active sessions"));
            }

            // Re-derive group keys alongside the snapshot rows: build_snapshot
            // groups by repo_root-or-cwd in most-recent order, so map headers
            // back to keys via the same states.
            let mut live: Vec<&state::SessionState> = states
                .iter()
                .filter(|s| now.saturating_sub(s.last_event_at) <= bar::HIDE_AFTER_SECS)
                .collect();
            live.sort_by_key(|s| std::cmp::Reverse(s.last_event_at));
            let mut grouped: Vec<(String, Vec<&state::SessionState>)> = Vec::new();
            for s in live {
                let key = s.repo_root.clone().unwrap_or_else(|| s.cwd.clone());
                match grouped.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, members)) => members.push(s),
                    None => grouped.push((key, vec![s])),
                }
            }

            for (group, (key, members)) in snap.groups.iter().zip(grouped.iter()) {
                menu.addItem(&section_header(mtm, &group.header));
                let mut sorted: Vec<&&state::SessionState> = members.iter().collect();
                sorted.sort_by_key(|s| (status_rank(s.status), std::cmp::Reverse(s.last_event_at)));
                for s in sorted {
                    let row = bar::build_row(s, key, now);
                    let item = NSMenuItem::new(mtm);
                    item.setTitle(&NSString::from_str(&row.label));
                    item.setImage(status_image(row.status).as_deref());
                    item.setToolTip(row.tooltip.as_deref().map(NSString::from_str).as_deref());
                    if row.enabled {
                        let target: &AnyObject = self;
                        unsafe {
                            item.setTarget(Some(target));
                            item.setAction(Some(sel!(focusSession:)));
                        }
                        item.setTag(actions.len() as isize);
                        actions.push(Action {
                            session_id: s.session_id.clone(),
                            group_key: key.clone(),
                            bundle_id: row.bundle_id.clone(),
                            open_path: row.open_path.clone(),
                        });
                    } else {
                        item.setEnabled(false);
                    }
                    menu.addItem(&item);
                }
            }

            menu.addItem(&NSMenuItem::separatorItem(mtm));
            self.add_history_submenu(menu, now);
            menu.addItem(&NSMenuItem::separatorItem(mtm));

            let login = NSMenuItem::new(mtm);
            login.setTitle(&NSString::from_str("Launch at Login"));
            let target: &AnyObject = self;
            unsafe {
                login.setTarget(Some(target));
                login.setAction(Some(sel!(toggleAgent:)));
            }
            login.setState(if agent::is_installed() { NSControlStateValueOn } else { NSControlStateValueOff });
            menu.addItem(&login);

            let quit = NSMenuItem::new(mtm);
            quit.setTitle(&NSString::from_str("Quit Tachi Bar"));
            unsafe {
                quit.setTarget(Some(target));
                quit.setAction(Some(sel!(quit:)));
            }
            quit.setKeyEquivalent(&NSString::from_str("q"));
            menu.addItem(&quit);

            *self.ivars().actions.borrow_mut() = actions;
        }

        fn add_history_submenu(&self, menu: &NSMenu, now: u64) {
            let mtm = self.mtm();
            let entries = history::tail(5);
            let parent = NSMenuItem::new(mtm);
            parent.setTitle(&NSString::from_str("Recent notifications"));
            let sub = NSMenu::new(mtm);
            if entries.is_empty() {
                sub.addItem(&disabled_item(mtm, "None yet"));
            }
            for e in entries.iter().rev() {
                let body: String = e.body.chars().take(60).collect();
                let line = format!("{} \u{00B7} {} \u{00B7} {}", history::rel_time(now, e.ts), e.repo, body);
                sub.addItem(&disabled_item(mtm, &line));
            }
            parent.setSubmenu(Some(&sub));
            menu.addItem(&parent);
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
    const TACHI_TEMPLATE: &[u8] = include_bytes!("../../assets/tachi-menubar-template.png");

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

    fn status_rank(status: state::Status) -> u8 {
        match status {
            state::Status::Waiting => 0,
            state::Status::Running => 1,
            state::Status::Idle => 2,
        }
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

    // Silence unused warning for Sel import used only in comparisons.
    #[allow(dead_code)]
    fn _sel_type(_: Sel) {}
}
