//! Renders the platform-neutral `bar::menu::Menu` into a muda menu and keeps
//! the id -> `Action` table the click handler needs.

use super::icon;
use std::collections::HashMap;
use tachi_noti::bar::menu::{Action, Item, Menu};
use tray_icon::menu::{
    CheckMenuItem, IconMenuItem, IsMenuItem, Menu as MudaMenu, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};

pub struct Rendered {
    pub menu: MudaMenu,
    pub actions: HashMap<MenuId, Action>,
}

/// Win32 reads `&` in a label as a mnemonic marker; repo names and notification
/// bodies can contain a literal one.
fn label(text: &str) -> String {
    text.replace('&', "&&")
}

struct Ctx {
    actions: HashMap<MenuId, Action>,
    next: u32,
}

impl Ctx {
    /// Fresh id bound to `action`. Ids are per render, so a stale event from a
    /// replaced menu simply finds no entry.
    fn bind(&mut self, action: &Action) -> MenuId {
        self.next += 1;
        let id = MenuId::new(format!("tachi-{}", self.next));
        self.actions.insert(id.clone(), action.clone());
        id
    }
}

fn boxed<T: IsMenuItem + 'static>(item: T) -> Box<dyn IsMenuItem> {
    Box::new(item)
}

fn build_item(item: &Item, ctx: &mut Ctx) -> Result<Box<dyn IsMenuItem>, String> {
    Ok(match item {
        Item::Header(text) | Item::Text(text) => boxed(MenuItem::new(label(text), false, None)),
        Item::Separator => boxed(PredefinedMenuItem::separator()),
        Item::Session(s) => {
            let dot = icon::menu_dot(s.row.status);
            let icon = tray_icon::menu::Icon::from_rgba(dot.data, dot.width, dot.height).ok();
            // Win32 menus have no hover tooltips; a waiting row's label already
            // carries its short hint, so the full detail is not rendered.
            let text = label(&s.row.label);
            match &s.action {
                Some(action) => boxed(IconMenuItem::with_id(ctx.bind(action), text, true, icon, None)),
                None => boxed(IconMenuItem::new(text, false, icon, None)),
            }
        }
        Item::Submenu { title, items } => {
            let children = build_items(items, ctx)?;
            let refs: Vec<&dyn IsMenuItem> = children.iter().map(|c| c.as_ref()).collect();
            boxed(Submenu::with_items(label(title), true, &refs).map_err(|e| e.to_string())?)
        }
        Item::Choice { title, checked, action } => {
            boxed(CheckMenuItem::with_id(ctx.bind(action), label(title), true, *checked, None))
        }
        // `key` is a macOS key equivalent; Win32 tray menus have no shortcuts.
        Item::Command { title, action, .. } => boxed(MenuItem::with_id(ctx.bind(action), label(title), true, None)),
    })
}

fn build_items(items: &[Item], ctx: &mut Ctx) -> Result<Vec<Box<dyn IsMenuItem>>, String> {
    items.iter().map(|i| build_item(i, ctx)).collect()
}

pub fn render(model: &Menu) -> Result<Rendered, String> {
    let mut ctx = Ctx { actions: HashMap::new(), next: 0 };
    let children = build_items(&model.items, &mut ctx)?;
    let refs: Vec<&dyn IsMenuItem> = children.iter().map(|c| c.as_ref()).collect();
    let menu = MudaMenu::with_items(&refs).map_err(|e| e.to_string())?;
    Ok(Rendered { menu, actions: ctx.actions })
}

/// Everything visible or clickable in the model, flattened in order. Equal
/// fingerprints mean an already-installed menu is still correct, so an open
/// menu is not swapped out from under the user for nothing.
pub fn fingerprint(model: &Menu) -> String {
    let mut out = String::new();
    push_items(&model.items, &mut out);
    out
}

fn push_items(items: &[Item], out: &mut String) {
    const SEP: char = '\u{1F}';
    for item in items {
        match item {
            Item::Header(t) => out.push_str(&format!("H{SEP}{t}{SEP}")),
            Item::Text(t) => out.push_str(&format!("T{SEP}{t}{SEP}")),
            Item::Separator => out.push_str(&format!("-{SEP}")),
            Item::Session(s) => out.push_str(&format!(
                "S{SEP}{}{SEP}{:?}{SEP}{}{SEP}{:?}{SEP}",
                s.row.label, s.row.status, s.row.enabled, s.action
            )),
            Item::Submenu { title, items } => {
                out.push_str(&format!("M{SEP}{title}{SEP}("));
                push_items(items, out);
                out.push(')');
            }
            Item::Choice { title, checked, action } => {
                out.push_str(&format!("C{SEP}{title}{SEP}{checked}{SEP}{action:?}{SEP}"))
            }
            Item::Command { title, action, .. } => out.push_str(&format!("B{SEP}{title}{SEP}{action:?}{SEP}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(checked: bool, extra: Option<&str>) -> Menu {
        let mut items = vec![
            Item::Text("No active sessions".into()),
            Item::Separator,
            Item::Submenu {
                title: "Completion Sound".into(),
                items: vec![Item::Choice { title: "Tachi Bark".into(), checked, action: Action::SetStopSound("TachiBark".into()) }],
            },
        ];
        if let Some(t) = extra {
            items.push(Item::Command { title: t.into(), action: Action::Quit, key: Some('q') });
        }
        Menu { items }
    }

    #[test]
    fn fingerprint_is_stable_for_equal_models() {
        assert_eq!(fingerprint(&menu(true, Some("Quit"))), fingerprint(&menu(true, Some("Quit"))));
    }

    #[test]
    fn fingerprint_sees_checked_flips_and_new_items() {
        let base = fingerprint(&menu(true, None));
        assert_ne!(base, fingerprint(&menu(false, None)));
        assert_ne!(base, fingerprint(&menu(true, Some("Quit"))));
    }

    #[test]
    fn fingerprint_distinguishes_a_text_line_from_a_header() {
        let a = Menu { items: vec![Item::Text("Usage".into())] };
        let b = Menu { items: vec![Item::Header("Usage".into())] };
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn ampersands_are_escaped_for_win32() {
        assert_eq!(label("a & b"), "a && b");
        assert_eq!(label("plain"), "plain");
    }
}
