//! Which windows commands can address, and by what name.
//!
//! The list a script sees (the `windows` command, `window{index}`) is the
//! registered windows in registration order followed by any other open
//! windows of the app, so an unregistered window is still reachable by index.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Result, bail};
use gpui::{AnyWindowHandle, App, Global};

use crate::protocol::WindowSelector;

#[derive(Debug, Default)]
pub(crate) struct WindowRegistry {
    /// Registered windows in registration order.
    named: Vec<(String, AnyWindowHandle)>,
    /// The window commands address when they carry no `window` field.
    selected: Option<AnyWindowHandle>,
}

/// Shared handle stored as a gpui global so hosts can (un)register windows
/// from anywhere with just `&mut App`.
#[derive(Clone, Default)]
pub(crate) struct SharedRegistry(pub Rc<RefCell<WindowRegistry>>);

impl Global for SharedRegistry {}

/// A window as listed to scripts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Listed {
    pub index: usize,
    pub name: Option<String>,
    pub handle: AnyWindowHandle,
}

impl WindowRegistry {
    pub fn register(&mut self, name: String, handle: AnyWindowHandle) {
        self.named
            .retain(|(existing, h)| *existing != name && *h != handle);
        self.named.push((name, handle));
    }

    pub fn unregister(&mut self, name: &str) {
        self.named.retain(|(existing, _)| existing != name);
    }

    pub fn name_of(&self, handle: AnyWindowHandle) -> Option<&str> {
        self.named
            .iter()
            .find(|(_, h)| *h == handle)
            .map(|(name, _)| name.as_str())
    }

    /// The script-visible list, given the app's currently open windows.
    pub fn list(&self, open: &[AnyWindowHandle]) -> Vec<Listed> {
        let registered = self
            .named
            .iter()
            .filter(|(_, handle)| open.contains(handle))
            .map(|(name, handle)| (Some(name.clone()), *handle));
        let unregistered = open
            .iter()
            .filter(|handle| !self.named.iter().any(|(_, h)| h == *handle))
            .map(|handle| (None, *handle));
        registered
            .chain(unregistered)
            .enumerate()
            .map(|(index, (name, handle))| Listed {
                index,
                name,
                handle,
            })
            .collect()
    }

    pub fn find(&self, selector: &WindowSelector, open: &[AnyWindowHandle]) -> Result<Listed> {
        let listed = self.list(open);
        let found = match selector {
            WindowSelector::Index(index) => listed.get(*index).cloned(),
            WindowSelector::Name(name) => listed
                .iter()
                .find(|entry| entry.name.as_deref() == Some(name.as_str()))
                .cloned(),
        };
        match found {
            Some(entry) => Ok(entry),
            None => bail!("no open window {selector} (open: {})", describe(&listed)),
        }
    }

    /// The window a command without a `window` field addresses: the
    /// selected one while it is open, otherwise the first listed window.
    pub fn current(&mut self, open: &[AnyWindowHandle]) -> Result<Listed> {
        let listed = self.list(open);
        if let Some(selected) = self.selected
            && let Some(entry) = listed.iter().find(|entry| entry.handle == selected)
        {
            return Ok(entry.clone());
        }
        let Some(first) = listed.first().cloned() else {
            bail!("the app has no open windows");
        };
        if self.selected.is_some() {
            log::warn!(
                "automation: selected window closed; falling back to {}",
                label(&first)
            );
        }
        self.selected = Some(first.handle);
        Ok(first)
    }

    pub fn select(&mut self, handle: AnyWindowHandle) {
        self.selected = Some(handle);
    }
}

pub(crate) fn label(entry: &Listed) -> String {
    match &entry.name {
        Some(name) => format!("{name:?} (#{})", entry.index),
        None => format!("#{}", entry.index),
    }
}

fn describe(listed: &[Listed]) -> String {
    if listed.is_empty() {
        return "none".into();
    }
    listed.iter().map(label).collect::<Vec<_>>().join(", ")
}

pub(crate) fn shared(cx: &App) -> Option<SharedRegistry> {
    cx.try_global::<SharedRegistry>().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::WindowId;
    use pretty_assertions::assert_eq;

    fn handle(id: u64) -> AnyWindowHandle {
        // Only identity matters here; the root view type is never inspected.
        gpui::WindowHandle::<gpui::Empty>::new(WindowId::from(id)).into()
    }

    #[test]
    fn registered_windows_come_first_in_registration_order() {
        let (a, b, c) = (handle(1), handle(2), handle(3));
        let mut registry = WindowRegistry::default();
        registry.register("reader".into(), b);
        registry.register("main".into(), a);
        let listed = registry.list(&[a, b, c]);
        let summary: Vec<(usize, Option<&str>)> = listed
            .iter()
            .map(|entry| (entry.index, entry.name.as_deref()))
            .collect();
        assert_eq!(summary, [(0, Some("reader")), (1, Some("main")), (2, None)]);
    }

    #[test]
    fn closed_windows_drop_out_of_the_list() {
        let (a, b) = (handle(1), handle(2));
        let mut registry = WindowRegistry::default();
        registry.register("main".into(), a);
        registry.register("reader".into(), b);
        let listed = registry.list(&[a]);
        assert_eq!(listed.len(), 1);
        assert!(
            registry
                .find(&WindowSelector::Name("reader".into()), &[a])
                .is_err()
        );
        assert_eq!(
            registry
                .find(&WindowSelector::Index(0), &[a])
                .unwrap()
                .handle,
            a
        );
    }

    #[test]
    fn re_registering_replaces_by_name_and_handle() {
        let (a, b) = (handle(1), handle(2));
        let mut registry = WindowRegistry::default();
        registry.register("main".into(), a);
        registry.register("main".into(), b);
        assert_eq!(registry.name_of(a), None);
        assert_eq!(registry.name_of(b), Some("main"));
        registry.register("renamed".into(), b);
        assert_eq!(registry.name_of(b), Some("renamed"));
        registry.unregister("renamed");
        assert_eq!(registry.name_of(b), None);
    }

    #[test]
    fn current_falls_back_when_the_selection_closes() {
        let (a, b) = (handle(1), handle(2));
        let mut registry = WindowRegistry::default();
        registry.register("main".into(), a);
        registry.register("reader".into(), b);
        registry.select(b);
        assert_eq!(registry.current(&[a, b]).unwrap().handle, b);
        assert_eq!(registry.current(&[a]).unwrap().handle, a);
        assert!(registry.current(&[]).is_err());
    }

    #[test]
    fn find_reports_what_is_open() {
        let a = handle(1);
        let mut registry = WindowRegistry::default();
        registry.register("main".into(), a);
        let err = registry
            .find(&WindowSelector::Name("reader".into()), &[a])
            .unwrap_err()
            .to_string();
        assert_eq!(err, "no open window \"reader\" (open: \"main\" (#0))");
    }
}
