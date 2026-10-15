use crate::config::Config;
use crate::notify::Notice;
use std::path::{Path, PathBuf};
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::HSTRING;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Toast `Group` for everything we send; paired with a per-session `Tag` so a
/// new toast for the same session replaces the previous one.
const TOAST_GROUP: &str = "tachi-noti";

#[derive(Debug)]
pub enum Backend {
    Toast,
}

impl Backend {
    pub fn name(&self) -> &'static str {
        "toast"
    }

    /// Doctor line suffix.
    pub fn detail(&self) -> String {
        let base = format!(" (WinRT, AppUserModelID {})", super::AUMID);
        if super::register::is_registered() {
            base
        } else {
            format!("{base} — identity not registered, run `tachi-noti register`")
        }
    }

    /// Printed after `tachi-noti test` when the backend needs user setup.
    pub fn setup_hint(&self) -> Option<&'static str> {
        if super::register::is_registered() {
            return None;
        }
        Some(
            "If nothing appeared: run `tachi-noti register` once (creates the Start menu shortcut\n\
             Windows requires before it shows toasts from a desktop app), then check\n\
             Settings → System → Notifications → Tachi is on and Focus assist is not hiding it.",
        )
    }
}

/// Windows has one delivery path; `config.backend` is ignored.
pub fn detect(_config: &Config) -> Backend {
    Backend::Toast
}

pub fn send(_backend: &Backend, n: &Notice) -> Result<()> {
    // WinRT needs an initialized apartment. S_FALSE (already initialized) and
    // RPC_E_CHANGED_MODE (host initialized it differently) are both fine here.
    // SAFETY: plain COM init with no reserved pointer.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    // Resolve the sound first: it decides whether the toast stays silent.
    let sound = resolve_sound(n.sound.as_deref());

    let xml = build_toast_xml(n, &sound);
    show(&xml, &n.group).map_err(|e| format!("toast failed: {e} — run `tachi-noti register`"))?;

    // Play after Show() succeeded, and to completion: the hook process exits
    // right after we return.
    if let SoundPlan::Own(path) = &sound {
        super::sound::play_blocking(path);
    }
    Ok(())
}

fn show(xml: &str, group: &str) -> windows::core::Result<()> {
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    toast.SetTag(&HSTRING::from(tag_for(group)))?;
    toast.SetGroup(&HSTRING::from(TOAST_GROUP))?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(super::AUMID))?;
    notifier.Show(&toast)
}

/// How the sound of a notification is produced.
#[derive(Debug, PartialEq, Eq)]
enum SoundPlan {
    /// Toast stays silent; we play this file ourselves (unpackaged apps
    /// cannot point a toast at a custom sound file).
    Own(PathBuf),
    /// No sound wanted.
    Silent,
    /// Unknown name: leave `<audio>` out so Windows plays its default —
    /// same outcome as macOS for an unknown sound name.
    SystemDefault,
}

fn resolve_sound(sound: Option<&str>) -> SoundPlan {
    match sound {
        None | Some("") => SoundPlan::Silent,
        Some(name) => match super::sound::preview_path(name) {
            Some(p) => SoundPlan::Own(p),
            None => SoundPlan::SystemDefault,
        },
    }
}

/// Toast `Tag` is limited to 64 chars and session ids may be longer, so use a
/// fixed-width FNV-1a hash (stable across runs, unlike `DefaultHasher`).
fn tag_for(group: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in group.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Notification text is user/agent controlled; it must never be able to close
/// an element or attribute.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 forbids most control characters even when escaped.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// `file:///C:/dir/a%20b.png` for a local path (verbatim prefix stripped,
/// forward slashes, everything outside the unreserved set percent-encoded).
fn file_uri(path: &Path) -> String {
    let raw = super::shell::normalize_command_path(&path.display().to_string());
    let raw = raw.trim_start_matches('/');
    let mut out = String::from("file:///");
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/' | b':') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn build_toast_xml(n: &Notice, sound: &SoundPlan) -> String {
    let mut launch_attrs = String::new();
    if let Some(host_id) = &n.activate {
        // Same rule as macOS: only VS Code-family hosts get window-precise focus.
        let per_folder = super::host::window_per_folder(host_id) || super::host::window_per_folder_hook_hint();
        let uri = crate::platform::activation_uri(&crate::platform::Activation {
            host_id: host_id.clone(),
            open_path: n.click_path.clone().filter(|_| per_folder),
            window: n.window.clone(),
        });
        launch_attrs = format!(" activationType=\"protocol\" launch=\"{}\"", xml_escape(&uri));
    }

    let mut texts = format!("<text>{}</text>", xml_escape(&n.title));
    if !n.subtitle.is_empty() {
        texts.push_str(&format!("<text>{}</text>", xml_escape(&n.subtitle)));
    }
    texts.push_str(&format!("<text>{}</text>", xml_escape(&n.body)));

    let image = n
        .icon
        .as_deref()
        .map(|p| {
            format!(
                "<image placement=\"appLogoOverride\" hint-crop=\"circle\" src=\"{}\"/>",
                xml_escape(&file_uri(Path::new(p)))
            )
        })
        .unwrap_or_default();

    let audio = match sound {
        SoundPlan::Own(_) | SoundPlan::Silent => "<audio silent=\"true\"/>",
        SoundPlan::SystemDefault => "",
    };

    format!(
        "<toast{launch_attrs} duration=\"short\"><visual><binding template=\"ToastGeneric\">{texts}{image}</binding></visual>{audio}</toast>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice() -> Notice {
        Notice {
            title: "Title".into(),
            subtitle: String::new(),
            body: "Body".into(),
            sound: None,
            group: "sess".into(),
            activate: None,
            click_path: None,
            window: None,
            icon: None,
        }
    }

    #[test]
    fn xml_escape_neutralizes_markup() {
        assert_eq!(xml_escape(r#"a&b<c>d"e'f"#), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
        assert_eq!(xml_escape("plain 中文"), "plain 中文");
        assert_eq!(xml_escape("a\u{1}b\tc"), "ab\tc", "forbidden control chars dropped");
    }

    #[test]
    fn hostile_text_cannot_inject_elements() {
        let mut n = notice();
        n.title = "</text><image src=\"x\"/>".into();
        let xml = build_toast_xml(&n, &SoundPlan::Silent);
        assert!(!xml.contains("<image"), "{xml}");
        assert!(xml.contains("&lt;/text&gt;"));
    }

    #[test]
    fn tag_is_stable_and_within_limit() {
        let long = "x".repeat(300);
        assert_eq!(tag_for(&long), tag_for(&long));
        assert_ne!(tag_for("a"), tag_for("b"));
        assert_eq!(tag_for(&long).len(), 16);
    }

    #[test]
    fn file_uri_encodes_and_normalizes() {
        assert_eq!(
            file_uri(Path::new(r"C:\Users\me\AppData\Local\tachi noti\head.png")),
            "file:///C:/Users/me/AppData/Local/tachi%20noti/head.png"
        );
        assert_eq!(file_uri(Path::new(r"\\?\C:\a\b.png")), "file:///C:/a/b.png");
    }

    #[test]
    fn sound_plan_controls_audio_element() {
        let n = notice();
        assert!(build_toast_xml(&n, &SoundPlan::Silent).contains("<audio silent=\"true\"/>"));
        assert!(build_toast_xml(&n, &SoundPlan::Own(PathBuf::from("x.wav"))).contains("<audio silent=\"true\"/>"));
        assert!(!build_toast_xml(&n, &SoundPlan::SystemDefault).contains("<audio"));
        assert_eq!(resolve_sound(None), SoundPlan::Silent);
        assert_eq!(resolve_sound(Some("")), SoundPlan::Silent);
    }

    #[test]
    fn subtitle_and_icon_are_optional() {
        let mut n = notice();
        let bare = build_toast_xml(&n, &SoundPlan::Silent);
        assert_eq!(bare.matches("<text>").count(), 2);
        assert!(!bare.contains("<image") && !bare.contains("activationType"));
        n.subtitle = "Sub".into();
        n.icon = Some(r"C:\a b\i.png".into());
        let full = build_toast_xml(&n, &SoundPlan::Silent);
        assert_eq!(full.matches("<text>").count(), 3);
        assert!(full.contains("src=\"file:///C:/a%20b/i.png\""));
    }
}
