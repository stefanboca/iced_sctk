mod state;

use std::{collections::BTreeMap, ffi::c_void, ptr::NonNull};

use iced_debug::core::{alignment, renderer, text, Color, Padding, Rectangle, Text, Vector};
use iced_program::{
    graphics::compositor,
    runtime::window::raw_window_handle::{
        DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
        RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle, WindowHandle,
    },
};
use rustc_hash::FxHashMap;
use sctk::{
    reexports::client::{
        protocol::{wl_display::WlDisplay, wl_surface::WlSurface},
        Proxy, QueueHandle,
    },
    shell::{wlr_layer::LayerSurface, WaylandSurface},
};
use state::State;

pub use crate::core::window::{Id, RedrawRequest};
use crate::{
    core::{input_method, mouse, theme, time::Instant, InputMethod, Point, Size},
    graphics::Compositor,
    program::{self, Program},
};

pub struct WindowManager<P>
where
    P: Program + 'static,
    P::Theme: theme::Base,
{
    aliases: FxHashMap<WlSurface, Id>,
    entries: BTreeMap<Id, Window<P>>,
}

impl<P> WindowManager<P>
where
    P: Program + 'static,
    P::Theme: theme::Base,
{
    pub fn new() -> Self {
        Self {
            aliases: FxHashMap::default(),
            entries: BTreeMap::new(),
        }
    }

    pub fn insert(
        &mut self,
        id: Id,
        qh: QueueHandle<crate::State<P>>,
        window: RawWindow,
        surface_size: Size<u32>,
        program: &program::Instance<P>,
        compositor: &mut <P::Renderer as compositor::Default>::Compositor,
    ) -> &mut Window<P> {
        let state = State::new(program, id, surface_size);
        let viewport_version = state.viewport_version();

        let surface =
            compositor.create_surface(window.clone(), surface_size.width, surface_size.height);
        let renderer = compositor.create_renderer();

        let _ = self.aliases.insert(window.surface().clone(), id);

        let _ = self.entries.insert(
            id,
            Window {
                qh,
                raw: window,
                state,
                viewport_version,
                surface,
                renderer,
                mouse_interaction: mouse::Interaction::None,
                redraw_at: RedrawRequest::Wait,
                preedit: None,
                ime_state: None,
            },
        );

        self.entries.get_mut(&id).unwrap()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn is_idle(&self) -> bool {
        self.entries
            .values()
            .all(|window| matches!(window.redraw_at, RedrawRequest::Wait))
    }

    pub fn redraw_at(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter_map(|window| match window.redraw_at {
                RedrawRequest::At(at) => Some(at),
                _ => None,
            })
            .min()
    }

    pub fn first(&self) -> Option<&Window<P>> {
        self.entries.first_key_value().map(|(_id, window)| window)
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Id, &mut Window<P>)> {
        self.entries.iter_mut().map(|(k, v)| (*k, v))
    }

    pub fn get(&self, id: Id) -> Option<&Window<P>> {
        self.entries.get(&id)
    }

    pub fn get_mut(&mut self, id: Id) -> Option<&mut Window<P>> {
        self.entries.get_mut(&id)
    }

    pub fn get_mut_alias(&mut self, surface: &WlSurface) -> Option<(Id, &mut Window<P>)> {
        let id = self.aliases.get(surface).copied()?;

        Some((id, self.get_mut(id)?))
    }

    pub fn remove(&mut self, id: Id) -> Option<Window<P>> {
        let window = self.entries.remove(&id)?;
        let _ = self.aliases.remove(window.raw.surface());

        Some(window)
    }
}

#[derive(Debug, Clone)]
pub enum RawWindow {
    Layer(WlDisplay, LayerSurface),
}

impl RawWindow {
    pub fn display(&self) -> &WlDisplay {
        match self {
            RawWindow::Layer(display, _) => display,
        }
    }

    pub fn surface(&self) -> &WlSurface {
        match self {
            RawWindow::Layer(_, layer_surface) => layer_surface.wl_surface(),
        }
    }
}

impl HasDisplayHandle for RawWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        let display = self.display().id().as_ptr() as *mut c_void;

        let c_ptr = NonNull::new(display).ok_or(HandleError::Unavailable)?;
        let handle = WaylandDisplayHandle::new(c_ptr);
        let raw_handle = RawDisplayHandle::Wayland(handle);
        #[allow(unsafe_code)]
        Ok(unsafe { DisplayHandle::borrow_raw(raw_handle) })
    }
}

impl HasWindowHandle for RawWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let surface = self.surface().id().as_ptr() as *mut c_void;
        let c_ptr = NonNull::new(surface).ok_or(HandleError::Unavailable)?;
        let handle = WaylandWindowHandle::new(c_ptr);
        let raw_handle = RawWindowHandle::Wayland(handle);
        #[allow(unsafe_code)]
        Ok(unsafe { WindowHandle::borrow_raw(raw_handle) })
    }
}

pub struct Window<P>
where
    P: Program + 'static,
    P::Theme: theme::Base,
{
    pub qh: QueueHandle<crate::State<P>>,
    pub raw: RawWindow,
    pub state: State<P>,
    pub viewport_version: u64,
    pub mouse_interaction: mouse::Interaction,
    pub surface: <<P::Renderer as compositor::Default>::Compositor as Compositor>::Surface,
    pub renderer: P::Renderer,
    pub redraw_at: RedrawRequest,
    preedit: Option<Preedit<P::Renderer>>,
    ime_state: Option<(Point, input_method::Purpose)>,
}

impl<P> Window<P>
where
    P: Program,
    P::Theme: theme::Base,
{
    pub fn size(&self) -> Size {
        self.state.logical_size()
    }

    pub fn request_redraw(&mut self, redraw_request: RedrawRequest) {
        if let RedrawRequest::NextFrame = self.redraw_at {
            return;
        }

        self.redraw_at = redraw_request;

        if let RedrawRequest::NextFrame = redraw_request {
            let _ = self
                .raw
                .surface()
                .frame(&self.qh, self.raw.surface().clone());
            self.raw.surface().commit();
        }
    }

    pub fn request_input_method(&mut self, input_method: InputMethod) {
        match input_method {
            InputMethod::Disabled => {
                self.disable_ime();
            }
            InputMethod::Enabled {
                position,
                purpose,
                preedit,
            } => {
                self.enable_ime(position, purpose);

                if let Some(preedit) = preedit {
                    if preedit.content.is_empty() {
                        self.preedit = None;
                    } else {
                        let mut overlay = self.preedit.take().unwrap_or_else(Preedit::new);

                        overlay.update(
                            position,
                            &preedit,
                            self.state.background_color(),
                            &self.renderer,
                        );

                        self.preedit = Some(overlay);
                    }
                } else {
                    self.preedit = None;
                }
            }
        }
    }

    pub fn update_mouse(&mut self, interaction: mouse::Interaction) {
        // TODO: set cursor

        self.mouse_interaction = interaction;
    }

    pub fn draw_preedit(&mut self) {
        if let Some(preedit) = &self.preedit {
            preedit.draw(
                &mut self.renderer,
                self.state.text_color(),
                self.state.background_color(),
                &Rectangle::new(Point::ORIGIN, self.state.viewport().logical_size()),
            );
        }
    }

    fn enable_ime(&mut self, position: Point, purpose: input_method::Purpose) {
        if self.ime_state.is_none() {
            // TODO: enable ime
        }

        if self.ime_state != Some((position, purpose)) {
            // TODO: configure position and purpose

            self.ime_state = Some((position, purpose));
        }
    }

    fn disable_ime(&mut self) {
        if self.ime_state.is_some() {
            // TOOD: enable ime
            self.ime_state = None;
        }

        self.preedit = None;
    }
}

struct Preedit<Renderer>
where
    Renderer: text::Renderer,
{
    position: Point,
    content: Renderer::Paragraph,
    spans: Vec<text::Span<'static, (), Renderer::Font>>,
}

impl<Renderer> Preedit<Renderer>
where
    Renderer: text::Renderer,
{
    fn new() -> Self {
        Self {
            position: Point::ORIGIN,
            spans: Vec::new(),
            content: Renderer::Paragraph::default(),
        }
    }

    fn update(
        &mut self,
        position: Point,
        preedit: &input_method::Preedit,
        background: Color,
        renderer: &Renderer,
    ) {
        self.position = position;

        let spans = match &preedit.selection {
            Some(selection) => {
                vec![
                    text::Span::new(&preedit.content[..selection.start]),
                    text::Span::new(if selection.start == selection.end {
                        "\u{200A}"
                    } else {
                        &preedit.content[selection.start..selection.end]
                    })
                    .color(background),
                    text::Span::new(&preedit.content[selection.end..]),
                ]
            }
            _ => vec![text::Span::new(&preedit.content)],
        };

        if spans != self.spans.as_slice() {
            use text::Paragraph as _;

            self.content = Renderer::Paragraph::with_spans(Text {
                content: &spans,
                bounds: Size::INFINITY,
                size: preedit.text_size.unwrap_or_else(|| renderer.default_size()),
                line_height: text::LineHeight::default(),
                font: renderer.default_font(),
                align_x: text::Alignment::Default,
                align_y: alignment::Vertical::Top,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::None,
            });

            self.spans.clear();
            self.spans
                .extend(spans.into_iter().map(text::Span::to_static));
        }
    }

    fn draw(&self, renderer: &mut Renderer, color: Color, background: Color, viewport: &Rectangle) {
        use text::Paragraph as _;

        if self.content.min_width() < 1.0 {
            return;
        }

        let mut bounds = Rectangle::new(
            self.position - Vector::new(0.0, self.content.min_height()),
            self.content.min_bounds(),
        );

        bounds.x = bounds
            .x
            .max(viewport.x)
            .min(viewport.x + viewport.width - bounds.width);

        bounds.y = bounds
            .y
            .max(viewport.y)
            .min(viewport.y + viewport.height - bounds.height);

        renderer.with_layer(bounds, |renderer| {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    ..Default::default()
                },
                background,
            );

            renderer.fill_paragraph(&self.content, bounds.position(), color, bounds);

            const UNDERLINE: f32 = 2.0;

            renderer.fill_quad(
                renderer::Quad {
                    bounds: bounds.shrink(Padding {
                        top: bounds.height - UNDERLINE,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                color,
            );

            for span_bounds in self.content.span_bounds(1) {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: span_bounds + (bounds.position() - Point::ORIGIN),
                        ..Default::default()
                    },
                    color,
                );
            }
        });
    }
}
