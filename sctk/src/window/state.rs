use std::fmt::{Debug, Formatter};

use iced_debug::core::Point;
use smithay_client_toolkit as sctk;

use crate::{
    core::{Color, Size, mouse, theme, window},
    graphics::Viewport,
    program::{self, Program},
};

/// The state of a multi-windowed [`Program`].
pub struct State<P: Program>
where
    P::Theme: theme::Base,
{
    title: String,
    scale_factor: f64,
    window_scale_factor: f64,
    viewport: Viewport,
    viewport_version: u64,
    cursor_position: Option<Point<f64>>,
    modifiers: sctk::seat::keyboard::Modifiers,
    theme: P::Theme,
    style: theme::Style,
}

impl<P: Program> Debug for State<P>
where
    P::Theme: theme::Base,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("multi_window::State")
            .field("title", &self.title)
            .field("scale_factor", &self.scale_factor)
            .field("viewport", &self.viewport)
            .field("viewport_version", &self.viewport_version)
            .field("cursor_position", &self.cursor_position)
            .field("style", &self.style)
            .finish()
    }
}

impl<P: Program> State<P>
where
    P::Theme: theme::Base,
{
    /// Creates a new [`State`] for the provided [`Program`]'s `window`.
    pub fn new(
        program: &program::Instance<P>,
        window_id: window::Id,
        physical_size: Size<u32>,
        window_scale_factor: f64,
    ) -> Self {
        let title = program.title(window_id);
        let scale_factor = program.scale_factor(window_id);
        let theme = program.theme(window_id);
        let style = program.style(&theme);

        let viewport =
            Viewport::with_physical_size(physical_size, window_scale_factor * scale_factor);

        Self {
            title,
            scale_factor,
            window_scale_factor,
            viewport,
            viewport_version: 0,
            cursor_position: None,
            modifiers: sctk::seat::keyboard::Modifiers::default(),
            theme,
            style,
        }
    }

    /// Returns the current [`Viewport`] of the [`State`].
    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    /// Returns the version of the [`Viewport`] of the [`State`].
    ///
    /// The version is incremented every time the [`Viewport`] changes.
    pub fn viewport_version(&self) -> u64 {
        self.viewport_version
    }

    /// Returns the physical [`Size`] of the [`Viewport`] of the [`State`].
    pub fn physical_size(&self) -> Size<u32> {
        self.viewport.physical_size()
    }

    /// Returns the logical [`Size`] of the [`Viewport`] of the [`State`].
    pub fn logical_size(&self) -> Size<f32> {
        self.viewport.logical_size()
    }

    pub fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    /// Returns the current cursor position of the [`State`].
    pub fn cursor(&self) -> mouse::Cursor {
        self.cursor_position
            .map(|p| {
                mouse::Cursor::Available(Point::new(
                    (p.x / self.scale_factor) as f32,
                    (p.y / self.scale_factor) as f32,
                ))
            })
            .unwrap_or(mouse::Cursor::Unavailable)
    }

    pub fn modifiers(&self) -> sctk::seat::keyboard::Modifiers {
        self.modifiers
    }

    /// Returns the current theme of the [`State`].
    pub fn theme(&self) -> &P::Theme {
        &self.theme
    }

    /// Returns the current background [`Color`] of the [`State`].
    pub fn background_color(&self) -> Color {
        self.style.background_color
    }

    /// Returns the current text [`Color`] of the [`State`].
    pub fn text_color(&self) -> Color {
        self.style.text_color
    }

    pub fn update_cursor(&mut self, position: Option<Point<f64>>) {
        self.cursor_position = position;
    }

    pub fn update_modifiers(&mut self, modifiers: sctk::seat::keyboard::Modifiers) {
        self.modifiers = modifiers;
    }

    pub fn resize(&mut self, physical_size: Size<u32>) {
        self.viewport = Viewport::with_physical_size(
            physical_size,
            self.window_scale_factor * self.scale_factor,
        );
        let _ = self.viewport_version.wrapping_add(1);
    }

    pub fn rescale(&mut self, window_scale_factor: f64) {
        self.window_scale_factor = window_scale_factor;
        self.viewport = Viewport::with_physical_size(
            self.viewport.physical_size(),
            window_scale_factor * self.scale_factor,
        );
        let _ = self.viewport_version.wrapping_add(1);
    }

    /// Synchronizes the [`State`] with its [`Program`] and its respective
    /// window.
    ///
    /// Normally, a [`Program`] should be synchronized with its [`State`]
    /// and window after calling [`State::update`].
    pub fn synchronize(&mut self, program: &program::Instance<P>, window_id: window::Id) {
        // Update window title
        let new_title = program.title(window_id);

        if self.title != new_title {
            // TODO: set title
            self.title = new_title;
        }

        let new_scale_factor = program.scale_factor(window_id);

        if self.scale_factor != new_scale_factor {
            self.viewport = Viewport::with_physical_size(
                self.viewport.physical_size(),
                self.window_scale_factor * new_scale_factor,
            );
            self.viewport_version = self.viewport_version.wrapping_add(1);

            self.scale_factor = new_scale_factor;
        }

        // Update theme and appearance
        self.theme = program.theme(window_id);
        self.style = program.style(&self.theme);
    }
}
