//! Same-document split workspace. View state is duplicated; the Session is never duplicated.

use egui::{Align, Layout, RichText, Ui, vec2};
use serde_json::{Value, json};

use crate::{WordApp, canvas};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane {
    #[default]
    Left,
    Right,
}

impl Pane {
    pub fn id(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Left => "A",
            Self::Right => "B",
        }
    }
}

#[derive(Default)]
pub struct Workspace {
    pub split: bool,
    pub active: Pane,
    /// The pane currently loaded in `WordApp.canvas`; the other is in `secondary_canvas`.
    loaded: Pane,
}

impl WordApp {
    fn load_pane(&mut self, pane: Pane) {
        if self.workspace.loaded != pane {
            std::mem::swap(&mut self.canvas, &mut self.secondary_canvas);
            self.workspace.loaded = pane;
        }
    }

    pub fn focus_pane(&mut self, pane: Pane) {
        self.workspace.active = pane;
        self.load_pane(pane);
    }

    pub fn set_split(&mut self, enabled: bool) {
        self.workspace.split = enabled;
        let active = self.workspace.active;
        self.load_pane(active);
    }

    /// A new active document must not inherit the other document's parked viewport.
    pub(crate) fn reset_secondary_viewport(&mut self) {
        self.secondary_canvas = canvas::CanvasState::default();
        self.workspace.active = self.workspace.loaded;
    }

    pub fn workspace_snapshot(&self) -> Value {
        let (left, right) = if self.workspace.loaded == Pane::Left {
            (&self.canvas, &self.secondary_canvas)
        } else {
            (&self.secondary_canvas, &self.canvas)
        };
        let pane = |id: Pane, canvas: &canvas::CanvasState| {
            json!({
                "id": id.id(),
                "viewedPage": canvas.viewed_page.map(|page| page + 1),
                "scale": canvas.scale / canvas::PX_PER_PT,
                "focused": id == self.workspace.active,
            })
        };
        json!({
            "split": self.workspace.split,
            "activePane": self.workspace.active.id(),
            "hasDocument": self.has_document(),
            "backstage": self.ui.backstage,
            "panes": [pane(Pane::Left, left), pane(Pane::Right, right)],
        })
    }
}

fn pane_chrome(app: &mut WordApp, ui: &mut Ui, pane: Pane) {
    ui.horizontal(|ui| {
        let active = app.workspace.active == pane;
        let marker = if active { "●" } else { "○" };
        let mut text = RichText::new(format!("{marker} {}", pane.label())).strong();
        if active {
            text = text.color(ui.visuals().selection.stroke.color);
        }
        ui.label(text);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(page) = app.canvas.viewed_page {
                ui.label(format!("{} {}", crate::tl!("Page"), page + 1));
            }
        });
    });
    ui.separator();
}

/// Draw both viewports through the existing canvas implementation. Keyboard/input state follows
/// the active pane; pointer press moves focus. The original active canvas is restored afterwards,
/// so chrome, shortcuts and host commands always see the user's active viewport.
pub fn show(app: &mut WordApp, ui: &mut Ui) {
    if !app.workspace.split {
        canvas::show(app, ui);
        return;
    }
    let available = ui.available_rect_before_wrap();
    let gap = 8.0;
    let width = ((available.width() - gap) / 2.0).max(80.0);
    let height = available.height().max(80.0);
    ui.horizontal(|ui| {
        for (index, pane) in [Pane::Left, Pane::Right].into_iter().enumerate() {
            if index > 0 {
                ui.add_space(gap);
            }
            ui.allocate_ui_with_layout(vec2(width, height), Layout::top_down(Align::Min), |ui| {
                let rect = ui.max_rect();
                let pressed = ui.input(|input| input.pointer.primary_pressed())
                    && ui.input(|input| input.pointer.interact_pos().is_some_and(|pos| rect.contains(pos)));
                if pressed {
                    app.workspace.active = pane;
                }
                app.load_pane(pane);
                app.canvas.focused = app.workspace.active == pane;
                pane_chrome(app, ui, pane);
                canvas::show(app, ui);
            });
        }
    });
    let active = app.workspace.active;
    app.load_pane(active);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_ids_are_explicit_and_bounded() {
        assert_eq!(Pane::parse("left"), Some(Pane::Left));
        assert_eq!(Pane::parse("right"), Some(Pane::Right));
        assert_eq!(Pane::parse("other"), None);
        assert_eq!(Pane::Left.id(), "left");
        assert_eq!(Pane::Right.id(), "right");
    }

    fn app() -> WordApp {
        let session = wordcraft_engine::Session::new(wordcraft_engine::sample::sample_document());
        WordApp::new(session, crate::Services::default())
    }

    #[test]
    fn viewport_scroll_is_independent_but_document_is_shared() {
        let mut app = app();
        let document = app.session.document_id();
        app.set_split(true);
        app.canvas.scroll_offset = vec2(0.0, 120.0);
        app.focus_pane(Pane::Right);
        app.canvas.scroll_offset = vec2(0.0, 640.0);
        app.focus_pane(Pane::Left);
        assert_eq!(app.canvas.scroll_offset, vec2(0.0, 120.0));
        assert_eq!(app.session.document_id(), document);
        app.focus_pane(Pane::Right);
        assert_eq!(app.canvas.scroll_offset, vec2(0.0, 640.0));
        assert_eq!(app.session.document_id(), document);
        assert_eq!(app.workspace_snapshot()["activePane"], "right");
    }

    #[test]
    fn disabling_split_keeps_the_active_viewport() {
        let mut app = app();
        app.set_split(true);
        app.focus_pane(Pane::Right);
        app.canvas.scroll_offset = vec2(0.0, 420.0);
        app.set_split(false);
        assert_eq!(app.workspace.active, Pane::Right);
        assert_eq!(app.canvas.scroll_offset, vec2(0.0, 420.0));
        assert!(!app.workspace_snapshot()["split"].as_bool().unwrap_or(true));
    }
}
