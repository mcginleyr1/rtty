//! A window and the panes in it. One pane shows at a time for now; tabs and splits will lay
//! several out, each in its own part of the window, and route focus between them.

use futures::channel::mpsc::UnboundedReceiver;
use gpui::{Context, Entity, Render, Window, div, prelude::*};

use crate::pane::{Notice, Pane};
use crate::view::{PaneEvent, PaneView};

pub struct TerminalWindow {
    panes: Vec<Entity<PaneView>>,
    active: usize,
}

impl TerminalWindow {
    pub fn new(
        pane: Pane,
        notices: UnboundedReceiver<Notice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = cx.new(|cx| PaneView::new(pane, notices, window, cx));
        cx.subscribe_in(&view, window, Self::pane_event).detach();
        TerminalWindow { panes: vec![view], active: 0 }
    }

    pub fn active(&self) -> Option<Entity<PaneView>> {
        self.panes.get(self.active).cloned()
    }

    /// The active pane names the window; a pane whose program exited goes, and the last one
    /// takes the app with it.
    fn pane_event(
        &mut self,
        pane: &Entity<PaneView>,
        event: &PaneEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = self.panes.iter().position(|view| view == pane);
        match event {
            PaneEvent::Title(title) if index == Some(self.active) => {
                window.set_window_title(if title.is_empty() { "rtty" } else { title })
            }
            PaneEvent::Title(_) => {}
            PaneEvent::Exited => {
                self.panes.retain(|view| view != pane);
                if self.panes.is_empty() {
                    cx.quit();
                    return;
                }
                self.active = self.active.min(self.panes.len() - 1);
                cx.notify();
            }
        }
    }
}

impl Render for TerminalWindow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.panes.get(self.active).cloned())
    }
}
