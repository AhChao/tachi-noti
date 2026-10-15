//! Windows front end: a notification-area icon whose context menu is the same
//! `bar::menu::Menu` the macOS menu bar renders. The tray lives on this
//! thread's plain Win32 message loop (tray-icon requires the creating thread
//! to pump messages); a thread timer drives the 2s heartbeat.

mod autostart;
mod console;
mod icon;
mod render;
mod tooltip;

use std::collections::HashMap;
use tachi_noti::bar::menu::{self, Action, Dispatched, Env};
use tachi_noti::hook::debug_log;
use tachi_noti::state::{self, Status};
use tray_icon::menu::{MenuEvent, MenuId};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, KillTimer, MSG, PostQuitMessage, SetTimer, TranslateMessage, WM_TIMER,
};

const HEARTBEAT_MS: u32 = 2000;
const FACE_PNG: &[u8] = include_bytes!("../../../assets/tachi-head.png");

/// `tachi-bar install-agent` on Windows.
pub fn install_autostart() {
    autostart::install();
}

/// `tachi-bar uninstall-agent` on Windows.
pub fn uninstall_autostart() {
    autostart::uninstall();
}

/// Let CLI subcommands print to the terminal that launched them.
pub fn attach_parent_console() {
    console::attach_parent();
}

/// Show the tray icon and run until Quit. Returns on exit.
pub fn run() {
    let face = icon::decode_png(FACE_PNG).unwrap_or_else(|e| {
        debug_log(&format!("tachi-bar: cannot decode the tray face: {e}"));
        icon::Rgba::transparent(icon::TRAY_SIZE, icon::TRAY_SIZE)
    });
    let mut app = App {
        base: icon::tray_base(&face),
        tray: None,
        actions: HashMap::new(),
        menu_fp: None,
        icon_key: None,
        tip: None,
    };
    if let Err(e) = app.create_tray() {
        debug_log(&format!("tachi-bar: cannot create the tray icon: {e}"));
        return;
    }
    app.refresh(true);

    // hwnd = NULL makes this a thread timer: WM_TIMER lands in this thread's
    // queue with a null msg.hwnd, and the returned id is the one in wParam.
    // SAFETY: no callback is registered, so nothing runs outside the loop below.
    let timer = unsafe { SetTimer(None, 0, HEARTBEAT_MS, None) };
    if timer == 0 {
        debug_log("tachi-bar: SetTimer failed; the tray will only update on menu actions");
    }

    let mut msg = MSG::default();
    loop {
        // SAFETY: msg is a valid out-pointer; 0 = WM_QUIT, -1 = error, both end the loop.
        if unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 <= 0 {
            break;
        }
        // SAFETY: msg was just filled by GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if timer != 0 && msg.message == WM_TIMER && msg.hwnd.0.is_null() && msg.wParam.0 == timer {
            app.refresh(false);
        }
        app.drain_events();
    }

    if timer != 0 {
        // SAFETY: the timer was created above on this thread.
        let _ = unsafe { KillTimer(None, timer) };
    }
}

struct App {
    /// Tachi's face at tray size, without any dots.
    base: icon::Rgba,
    /// None only between a Quit / Restart and the process going away.
    tray: Option<TrayIcon>,
    /// Click targets of the menu currently installed.
    actions: HashMap<MenuId, Action>,
    /// What is currently installed, so unchanged state costs no tray updates.
    menu_fp: Option<String>,
    icon_key: Option<(Option<Status>, bool)>,
    tip: Option<String>,
}

impl App {
    fn create_tray(&mut self) -> Result<(), String> {
        let img = icon::compose_tray(&self.base, None, false);
        let image = tray_icon::Icon::from_rgba(img.data, img.width, img.height).map_err(|e| e.to_string())?;
        let tray = TrayIconBuilder::new()
            .with_icon(image)
            .with_tooltip(tooltip::text(&[], false))
            .with_menu_on_left_click(true)
            .with_menu_on_right_click(true)
            .build()
            .map_err(|e| e.to_string())?;
        self.tray = Some(tray);
        // A fresh icon has none of the cached state applied.
        self.menu_fp = None;
        self.icon_key = None;
        self.tip = None;
        self.actions.clear();
        Ok(())
    }

    /// Re-read session state and push only what changed to the tray. `force`
    /// reapplies everything (after a toggle, or a re-created icon).
    fn refresh(&mut self, force: bool) {
        let Some(tray) = &self.tray else { return };
        let states = state::reap_dead(state::load_all());
        let now = state::now_epoch();
        let (segments, alert) = menu::title(&states, now);
        let model = menu::build(states, now, &Env { launch_at_login: autostart::is_enabled() });

        let dot = segments.first().map(|(_, s)| *s);
        let key = (dot, alert);
        if force || self.icon_key != Some(key) {
            let img = icon::compose_tray(&self.base, dot, alert);
            match tray_icon::Icon::from_rgba(img.data, img.width, img.height) {
                Ok(image) => match tray.set_icon(Some(image)) {
                    Ok(()) => self.icon_key = Some(key),
                    Err(e) => debug_log(&format!("tachi-bar: set_icon failed: {e}")),
                },
                Err(e) => debug_log(&format!("tachi-bar: bad tray icon: {e}")),
            }
        }

        let tip = tooltip::text(&segments, alert);
        if force || self.tip.as_deref() != Some(tip.as_str()) {
            match tray.set_tooltip(Some(&tip)) {
                Ok(()) => self.tip = Some(tip),
                Err(e) => debug_log(&format!("tachi-bar: set_tooltip failed: {e}")),
            }
        }

        let fp = render::fingerprint(&model);
        if force || self.menu_fp.as_deref() != Some(fp.as_str()) {
            match render::render(&model) {
                Ok(r) => {
                    tray.set_menu(Some(Box::new(r.menu)));
                    self.actions = r.actions;
                    self.menu_fp = Some(fp);
                }
                Err(e) => debug_log(&format!("tachi-bar: cannot render the menu: {e}")),
            }
        }
    }

    fn drain_events(&mut self) {
        // Nobody listens to hover / click events, but tray-icon queues them
        // unboundedly; empty the channel so it cannot grow for the process lifetime.
        while TrayIconEvent::receiver().try_recv().is_ok() {}
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            self.on_menu_event(&event.id);
        }
    }

    fn on_menu_event(&mut self, id: &MenuId) {
        let Some(action) = self.actions.get(id).cloned() else { return };
        match menu::dispatch_shared(&action) {
            Dispatched::Done => self.refresh(false),
            Dispatched::ToggleLaunchAtLogin => {
                let result = if autostart::is_enabled() { autostart::disable().map(|_| ()) } else { autostart::enable().map(|_| ()) };
                if let Err(e) = result {
                    debug_log(&format!("tachi-bar: cannot toggle launch at login: {e}"));
                }
                self.refresh(true);
            }
            Dispatched::Restart => {
                // Remove our icon first: process::exit would leave a ghost in
                // the notification area until the mouse passes over it.
                self.tray = None;
                crate::restart();
                // Only reached when restart failed; stay usable.
                if let Err(e) = self.create_tray() {
                    debug_log(&format!("tachi-bar: cannot re-create the tray icon: {e}"));
                    // SAFETY: plain call on the loop thread.
                    unsafe { PostQuitMessage(1) };
                    return;
                }
                self.refresh(true);
            }
            Dispatched::Quit => {
                self.tray = None; // drops the icon immediately
                // SAFETY: plain call on the loop thread.
                unsafe { PostQuitMessage(0) };
            }
        }
    }
}
