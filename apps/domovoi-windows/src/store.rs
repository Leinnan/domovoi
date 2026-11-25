//! The view model as the window draws it: the core's `ViewModel`, kept
//! current with its deltas, and split into `Rc` parts. A delta names only the
//! sections that changed, so only those parts are replaced; reactor skips a
//! component whose input is the same `Rc` as before.

use std::rc::Rc;

use domovoi_core::delta::ViewDelta;
use domovoi_core::view::{ComposerView, ConnectionView, SidebarView, ThreadPageView, ToastView, ViewModel};

#[derive(Default)]
pub struct Store {
    vm: ViewModel,
    pub connection: Rc<ConnectionView>,
    pub sidebar: Rc<SidebarView>,
    pub thread: Option<Rc<ThreadPageView>>,
    pub composer: Rc<ComposerView>,
    pub toast: Option<Rc<ToastView>>,
    pub route: Rc<str>,
}

/// Which parts a delta replaced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Changed {
    pub connection: bool,
    pub sidebar: bool,
    pub thread: bool,
    pub composer: bool,
}

impl Store {
    pub fn apply(&mut self, delta: ViewDelta) -> Changed {
        let changed = Changed {
            connection: delta.connection.is_some(),
            sidebar: delta.sidebar.is_some(),
            thread: delta.thread.is_some(),
            composer: delta.composer.is_some(),
        };
        let (toast, route) = (delta.toast.is_some(), delta.route.is_some());
        delta.apply(&mut self.vm);
        if changed.connection {
            self.connection = Rc::new(self.vm.connection.clone());
        }
        if changed.sidebar {
            self.sidebar = Rc::new(self.vm.sidebar.clone());
        }
        if changed.thread {
            self.thread = self.vm.thread.clone().map(Rc::new);
        }
        if changed.composer {
            self.composer = Rc::new(self.vm.composer.clone());
        }
        if toast {
            self.toast = self.vm.toast.clone().map(Rc::new);
        }
        if route {
            self.route = self.vm.route.as_str().into();
        }
        changed
    }

    /// The pairing form is shown instead of the threads.
    pub fn needs_pairing(&self) -> bool {
        let c = &self.connection;
        c.needs_pairing || c.environments.is_empty() && c.state != "connected"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_parts_a_delta_names_are_replaced() {
        let mut store = Store::default();
        let before = ViewModel::default();
        let mut after = before.clone();
        after.connection.state = "connected".into();
        let changed = store.apply(ViewDelta::between(Some(&before), &after));
        assert_eq!(changed, Changed { connection: true, ..Changed::default() });
        assert_eq!(store.connection.state, "connected");
        let sidebar = Rc::clone(&store.sidebar);
        let mut later = after.clone();
        later.connection.error = Some("gone".into());
        store.apply(ViewDelta::between(Some(&after), &later));
        assert!(Rc::ptr_eq(&sidebar, &store.sidebar), "the sidebar is untouched, so the same Rc");
        assert_eq!(store.connection.error.as_deref(), Some("gone"));
    }

    #[test]
    fn a_new_device_pairs_first() {
        assert!(Store::default().needs_pairing());
    }
}
